/*
 * A small, stable C interface over FFmpeg for Lasair's video input (PRD A23).
 *
 * Compiled against whatever FFmpeg headers are installed (6.x to 8.x) and
 * linked to FFmpeg's shared libraries, so the libraries stay replaceable
 * (LGPL) and no struct layout is written by hand on the Rust side.
 *
 * One handle decodes one video stream. Opening scans the stream's packets
 * (no decoding) to learn every frame's presentation time and the keyframes,
 * so frames can be addressed by index: sequential requests decode forward,
 * others seek to the nearest keyframe before the target. Frames come out
 * as straight RGBA8, converted with the stream's colour matrix and range.
 * Hardware decoding (VideoToolbox, D3D11VA, VAAPI) is used when the
 * platform and codec support it; frames are copied back to the CPU.
 *
 * Not thread-safe: one thread per handle.
 */

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct lsr_video {
    AVFormatContext *fmt;
    AVCodecContext *dec;
    AVBufferRef *hw_device;
    enum AVPixelFormat hw_format;
    int stream;
    AVPacket *pkt;
    AVFrame *frame;
    AVFrame *sw_frame;
    struct SwsContext *sws;
    /* Presentation times (stream time base), sorted, and keyframe flags. */
    int64_t *pts;
    uint8_t *key;
    int64_t count;
    double *times;
    /* Index of the frame the decoder will deliver next, or -1 after a seek. */
    int64_t next;
    /* Output size, fixed at open: frames of another size are scaled to it. */
    int width, height;
    int eof;
    char codec[128];
    char hw_name[32];
} lsr_video;

static void set_err(char *err, int len, const char *what, int code) {
    if (!err || len <= 0) return;
    if (code < 0) {
        char buf[AV_ERROR_MAX_STRING_SIZE] = {0};
        av_strerror(code, buf, sizeof buf);
        snprintf(err, (size_t)len, "%s: %s", what, buf);
    } else {
        snprintf(err, (size_t)len, "%s", what);
    }
}

static int cmp_i64(const void *a, const void *b) {
    int64_t x = *(const int64_t *)a, y = *(const int64_t *)b;
    return (x > y) - (x < y);
}

static enum AVPixelFormat pick_hw_format(AVCodecContext *ctx, const enum AVPixelFormat *fmts) {
    lsr_video *v = (lsr_video *)ctx->opaque;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++) {
        if (*p == v->hw_format) return *p;
    }
    /* The hardware cannot decode this stream: fall back to software. */
    v->hw_name[0] = 0;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++) {
        const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(*p);
        if (d && !(d->flags & AV_PIX_FMT_FLAG_HWACCEL)) return *p;
    }
    return AV_PIX_FMT_NONE;
}

static void try_hardware(lsr_video *v, const AVCodec *codec) {
    const enum AVHWDeviceType wanted[] = {
#if defined(__APPLE__)
        AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
#elif defined(_WIN32)
        AV_HWDEVICE_TYPE_D3D11VA,
        AV_HWDEVICE_TYPE_DXVA2,
#else
        AV_HWDEVICE_TYPE_VAAPI,
#endif
        AV_HWDEVICE_TYPE_NONE,
    };
    for (int w = 0; wanted[w] != AV_HWDEVICE_TYPE_NONE; w++) {
        for (int i = 0;; i++) {
            const AVCodecHWConfig *cfg = avcodec_get_hw_config(codec, i);
            if (!cfg) break;
            if (!(cfg->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) ||
                cfg->device_type != wanted[w])
                continue;
            AVBufferRef *dev = NULL;
            if (av_hwdevice_ctx_create(&dev, wanted[w], NULL, NULL, 0) < 0) break;
            v->hw_device = dev;
            v->hw_format = cfg->pix_fmt;
            v->dec->hw_device_ctx = av_buffer_ref(dev);
            v->dec->opaque = v;
            v->dec->get_format = pick_hw_format;
            snprintf(v->hw_name, sizeof v->hw_name, "%s", av_hwdevice_get_type_name(wanted[w]));
            return;
        }
    }
}

void lsr_video_close(lsr_video *v) {
    if (!v) return;
    sws_freeContext(v->sws);
    av_frame_free(&v->frame);
    av_frame_free(&v->sw_frame);
    av_packet_free(&v->pkt);
    avcodec_free_context(&v->dec);
    av_buffer_unref(&v->hw_device);
    avformat_close_input(&v->fmt);
    free(v->pts);
    free(v->key);
    free(v->times);
    free(v);
}

/* Records every packet's time and keyframe flag, then rewinds. */
static int scan(lsr_video *v, char *err, int errlen) {
    int64_t cap = 256;
    v->pts = malloc((size_t)cap * sizeof *v->pts);
    v->key = malloc((size_t)cap);
    if (!v->pts || !v->key) {
        set_err(err, errlen, "out of memory", 0);
        return -1;
    }
    int r;
    while ((r = av_read_frame(v->fmt, v->pkt)) >= 0) {
        if (v->pkt->stream_index == v->stream) {
            int64_t t = v->pkt->pts != AV_NOPTS_VALUE ? v->pkt->pts : v->pkt->dts;
            if (t != AV_NOPTS_VALUE) {
                if (v->count == cap) {
                    cap *= 2;
                    int64_t *p = realloc(v->pts, (size_t)cap * sizeof *p);
                    if (p) v->pts = p;
                    uint8_t *k = p ? realloc(v->key, (size_t)cap) : NULL;
                    if (k) v->key = k;
                    if (!p || !k) {
                        set_err(err, errlen, "out of memory", 0);
                        av_packet_unref(v->pkt);
                        return -1;
                    }
                }
                v->pts[v->count] = t;
                v->key[v->count] = (v->pkt->flags & AV_PKT_FLAG_KEY) ? 1 : 0;
                v->count++;
            }
        }
        av_packet_unref(v->pkt);
    }
    if (r != AVERROR_EOF) {
        set_err(err, errlen, "reading packets", r);
        return -1;
    }
    if (v->count == 0) {
        set_err(err, errlen, "the video stream has no frames", 0);
        return -1;
    }
    /* Sort (pts, key) pairs by pts: decode order becomes display order. */
    int64_t *pairs = malloc((size_t)v->count * 2 * sizeof *pairs);
    if (!pairs) {
        set_err(err, errlen, "out of memory", 0);
        return -1;
    }
    for (int64_t i = 0; i < v->count; i++) {
        pairs[2 * i] = v->pts[i];
        pairs[2 * i + 1] = v->key[i];
    }
    qsort(pairs, (size_t)v->count, 2 * sizeof *pairs, cmp_i64);
    AVStream *st = v->fmt->streams[v->stream];
    v->times = malloc((size_t)v->count * sizeof *v->times);
    if (!v->times) {
        free(pairs);
        set_err(err, errlen, "out of memory", 0);
        return -1;
    }
    int64_t start = pairs[0];
    for (int64_t i = 0; i < v->count; i++) {
        v->pts[i] = pairs[2 * i];
        v->key[i] = (uint8_t)pairs[2 * i + 1];
        v->times[i] = (double)(v->pts[i] - start) * av_q2d(st->time_base);
    }
    v->key[0] = 1;
    free(pairs);
    r = av_seek_frame(v->fmt, v->stream, v->pts[0], AVSEEK_FLAG_BACKWARD);
    if (r < 0) {
        set_err(err, errlen, "rewinding", r);
        return -1;
    }
    v->next = 0;
    return 0;
}

lsr_video *lsr_video_open(const char *path, int hardware, char *err, int errlen) {
    /* Errors come back as messages; FFmpeg's own log stays quiet. */
    av_log_set_level(AV_LOG_FATAL);
    lsr_video *v = calloc(1, sizeof *v);
    if (!v) {
        set_err(err, errlen, "out of memory", 0);
        return NULL;
    }
    v->hw_format = AV_PIX_FMT_NONE;
    int r = avformat_open_input(&v->fmt, path, NULL, NULL);
    if (r < 0) {
        set_err(err, errlen, "opening", r);
        goto fail;
    }
    r = avformat_find_stream_info(v->fmt, NULL);
    if (r < 0) {
        set_err(err, errlen, "reading stream info", r);
        goto fail;
    }
    const AVCodec *codec = NULL;
    r = av_find_best_stream(v->fmt, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    if (r < 0) {
        set_err(err, errlen, r == AVERROR_DECODER_NOT_FOUND ? "no decoder for this video"
                                                              : "no video stream",
                0);
        goto fail;
    }
    v->stream = r;
    AVStream *st = v->fmt->streams[v->stream];
    for (unsigned i = 0; i < v->fmt->nb_streams; i++) {
        if ((int)i != v->stream) v->fmt->streams[i]->discard = AVDISCARD_ALL;
    }
    v->dec = avcodec_alloc_context3(codec);
    v->pkt = av_packet_alloc();
    v->frame = av_frame_alloc();
    v->sw_frame = av_frame_alloc();
    if (!v->dec || !v->pkt || !v->frame || !v->sw_frame) {
        set_err(err, errlen, "out of memory", 0);
        goto fail;
    }
    r = avcodec_parameters_to_context(v->dec, st->codecpar);
    if (r < 0) {
        set_err(err, errlen, "decoder parameters", r);
        goto fail;
    }
    v->dec->thread_count = 0;
    v->dec->pkt_timebase = st->time_base;
    if (hardware) try_hardware(v, codec);
    r = avcodec_open2(v->dec, codec, NULL);
    if (r < 0 && v->hw_device) {
        /* Hardware set-up failed late: retry in software. */
        avcodec_free_context(&v->dec);
        av_buffer_unref(&v->hw_device);
        v->hw_name[0] = 0;
        v->dec = avcodec_alloc_context3(codec);
        if (v->dec && avcodec_parameters_to_context(v->dec, st->codecpar) >= 0) {
            v->dec->thread_count = 0;
            v->dec->pkt_timebase = st->time_base;
            r = avcodec_open2(v->dec, codec, NULL);
        }
    }
    if (r < 0) {
        set_err(err, errlen, "opening the decoder", r);
        goto fail;
    }
    v->width = v->dec->width;
    v->height = v->dec->height;
    if (v->width <= 0 || v->height <= 0 || v->width > 16384 || v->height > 16384) {
        set_err(err, errlen, "the video has no usable size", 0);
        goto fail;
    }
    snprintf(v->codec, sizeof v->codec, "%s", codec->long_name ? codec->long_name : codec->name);
    if (scan(v, err, errlen) < 0) goto fail;
    return v;
fail:
    lsr_video_close(v);
    return NULL;
}

void lsr_video_info(const lsr_video *v, int *width, int *height, int64_t *frames,
                    const double **times, const char **codec, const char **hw) {
    *width = v->width;
    *height = v->height;
    *frames = v->count;
    *times = v->times;
    *codec = v->codec;
    *hw = v->hw_name;
}

static int64_t index_of(const lsr_video *v, int64_t t) {
    int64_t lo = 0, hi = v->count - 1;
    while (lo < hi) {
        int64_t mid = (lo + hi + 1) / 2;
        if (v->pts[mid] <= t)
            lo = mid;
        else
            hi = mid - 1;
    }
    return lo;
}

static int convert(lsr_video *v, AVFrame *f, uint8_t *rgba, char *err, int errlen) {
    int w = v->width, h = v->height;
    v->sws = sws_getCachedContext(v->sws, f->width, f->height, (enum AVPixelFormat)f->format, w,
                                  h, AV_PIX_FMT_RGBA, SWS_BICUBIC, NULL, NULL, NULL);
    if (!v->sws) {
        set_err(err, errlen, "no conversion from this pixel format", 0);
        return -1;
    }
    int cs = f->colorspace == AVCOL_SPC_UNSPECIFIED
                 ? (h >= 720 ? SWS_CS_ITU709 : SWS_CS_ITU601)
                 : (int)f->colorspace;
    int full = f->color_range == AVCOL_RANGE_JPEG;
    int *inv, in_full, *table, out_full, brightness, contrast, saturation;
    if (sws_getColorspaceDetails(v->sws, &inv, &in_full, &table, &out_full, &brightness,
                                 &contrast, &saturation) >= 0) {
        sws_setColorspaceDetails(v->sws, sws_getCoefficients(cs), full, table, 1, brightness,
                                 contrast, saturation);
    }
    uint8_t *dst[4] = {rgba, NULL, NULL, NULL};
    int stride[4] = {w * 4, 0, 0, 0};
    sws_scale(v->sws, (const uint8_t *const *)f->data, f->linesize, 0, f->height, dst, stride);
    return 0;
}

/* Decodes frames until the one with index >= `index` (or the end). Returns
 * 1 when it was converted into `rgba`, 0 at the end, -1 on error. */
static int decode_to(lsr_video *v, int64_t index, uint8_t *rgba, char *err, int errlen) {
    for (;;) {
        int r = avcodec_receive_frame(v->dec, v->frame);
        if (r == 0) {
            int64_t t = v->frame->best_effort_timestamp;
            if (t == AV_NOPTS_VALUE) t = v->frame->pts;
            int64_t got = t == AV_NOPTS_VALUE ? (v->next < 0 ? 0 : v->next) : index_of(v, t);
            v->next = got + 1;
            if (got < index) {
                av_frame_unref(v->frame);
                continue;
            }
            AVFrame *f = v->frame;
            if (v->hw_device && f->format == v->hw_format) {
                r = av_hwframe_transfer_data(v->sw_frame, f, 0);
                if (r < 0) {
                    av_frame_unref(v->frame);
                    set_err(err, errlen, "copying the frame from the GPU", r);
                    return -1;
                }
                v->sw_frame->colorspace = f->colorspace;
                v->sw_frame->color_range = f->color_range;
                f = v->sw_frame;
            }
            r = convert(v, f, rgba, err, errlen);
            av_frame_unref(v->sw_frame);
            av_frame_unref(v->frame);
            return r < 0 ? -1 : 1;
        }
        if (r == AVERROR_EOF) return 0;
        if (r != AVERROR(EAGAIN)) {
            set_err(err, errlen, "decoding", r);
            return -1;
        }
        if (v->eof) return 0;
        /* The decoder wants input. */
        for (;;) {
            r = av_read_frame(v->fmt, v->pkt);
            if (r == AVERROR_EOF) {
                v->eof = 1;
                avcodec_send_packet(v->dec, NULL);
                break;
            }
            if (r < 0) {
                set_err(err, errlen, "reading", r);
                return -1;
            }
            if (v->pkt->stream_index != v->stream) {
                av_packet_unref(v->pkt);
                continue;
            }
            r = avcodec_send_packet(v->dec, v->pkt);
            av_packet_unref(v->pkt);
            if (r < 0 && r != AVERROR(EAGAIN) && r != AVERROR_INVALIDDATA) {
                set_err(err, errlen, "decoding", r);
                return -1;
            }
            break;
        }
    }
}

/* Decodes frame `index` (0-based, display order) into `rgba` (width x height
 * x 4 bytes). Returns 0 on success, -1 with a message in `err`. */
int lsr_video_frame(lsr_video *v, int64_t index, uint8_t *rgba, char *err, int errlen) {
    if (index < 0 || index >= v->count) {
        set_err(err, errlen, "frame out of range", 0);
        return -1;
    }
    /* Decoding forward is cheaper than seeking for a short way. */
    int forward = v->next >= 0 && index >= v->next && index - v->next < 48 && !v->eof;
    if (!forward) {
        int64_t k = index;
        while (k > 0 && !v->key[k]) k--;
        int r = av_seek_frame(v->fmt, v->stream, v->pts[k], AVSEEK_FLAG_BACKWARD);
        if (r < 0) {
            set_err(err, errlen, "seeking", r);
            return -1;
        }
        avcodec_flush_buffers(v->dec);
        v->eof = 0;
        v->next = -1;
    }
    int r = decode_to(v, index, rgba, err, errlen);
    if (r == 0) {
        set_err(err, errlen, "the video ended before this frame", 0);
        return -1;
    }
    return r < 0 ? -1 : 0;
}

/* libavcodec's version, as AV_VERSION_INT. */
unsigned lsr_video_avcodec_version(void) { return avcodec_version(); }
