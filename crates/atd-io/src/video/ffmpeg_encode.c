/*
 * A small, stable C interface over FFmpeg for Atidarla's video output
 * (FR-S10, PRD A51): RGBA8 frames in, one video stream in a .mov or .mp4 out.
 *
 * Like the decoding shim (ffmpeg_shim.c) it is compiled against the
 * installed FFmpeg headers and linked to FFmpeg's shared libraries, so the
 * libraries stay replaceable (LGPL). The encoder is chosen by name by the
 * caller, which only ever names LGPL encoders: prores_ks, and the
 * platform's hardware H.264/H.265 encoders (VideoToolbox, NVENC, Media
 * Foundation). GPL encoders (libx264, libx265) are never requested, so an
 * LGPL FFmpeg build is enough.
 *
 * Frames are converted from full-range RGBA to the encoder's YUV format
 * with the BT.709 matrix at limited range, and the stream is tagged
 * BT.709 so decoders convert back with the same matrix.
 *
 * Not thread-safe: one thread per handle.
 */

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct atd_encoder {
    AVFormatContext *fmt;
    AVCodecContext *enc;
    AVStream *st;
    AVFrame *frame;
    /* Describes the caller's RGBA buffer (no data of its own). */
    AVFrame *rgba;
    AVPacket *pkt;
    struct SwsContext *sws;
    int64_t next_pts;
    int width, height;
    int header_written;
} atd_encoder;

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

/* 1 when the linked FFmpeg has an encoder of this name. */
int atd_encoder_available(const char *name) {
    return avcodec_find_encoder_by_name(name) != NULL;
}

/* The software pixel formats the encoder accepts, terminated by NONE. */
static const enum AVPixelFormat *pixel_formats(const AVCodec *codec, AVCodecContext *ctx) {
#if LIBAVCODEC_VERSION_INT >= AV_VERSION_INT(61, 13, 100)
    const void *list = NULL;
    int n = 0;
    if (avcodec_get_supported_config(ctx, codec, AV_CODEC_CONFIG_PIX_FORMAT, 0, &list, &n) < 0)
        return NULL;
    return (const enum AVPixelFormat *)list;
#else
    (void)ctx;
    return codec->pix_fmts;
#endif
}

static enum AVPixelFormat choose_format(const AVCodec *codec, AVCodecContext *ctx, int alpha) {
    if (!strcmp(codec->name, "prores_ks"))
        return alpha ? AV_PIX_FMT_YUVA444P10LE : AV_PIX_FMT_YUV422P10LE;
    const enum AVPixelFormat *fmts = pixel_formats(codec, ctx);
    if (!fmts) return AV_PIX_FMT_YUV420P;
    const enum AVPixelFormat preferred[] = {AV_PIX_FMT_NV12, AV_PIX_FMT_YUV420P};
    for (size_t i = 0; i < sizeof preferred / sizeof preferred[0]; i++) {
        for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++) {
            if (*p == preferred[i]) return *p;
        }
    }
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++) {
        const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(*p);
        if (d && !(d->flags & AV_PIX_FMT_FLAG_HWACCEL)) return *p;
    }
    return AV_PIX_FMT_NONE;
}

void atd_encoder_free(atd_encoder *e) {
    if (!e) return;
    sws_freeContext(e->sws);
    av_frame_free(&e->frame);
    av_frame_free(&e->rgba);
    av_packet_free(&e->pkt);
    avcodec_free_context(&e->enc);
    if (e->fmt) {
        if (e->fmt->pb && !(e->fmt->oformat->flags & AVFMT_NOFILE)) avio_closep(&e->fmt->pb);
        avformat_free_context(e->fmt);
    }
    free(e);
}

/* Opens `path` (container from the extension) for `width` x `height`
 * frames at fps_num/fps_den with the encoder `name`. `profile` is the
 * ProRes profile for prores_ks (2 standard, 3 HQ, 4 4444 with alpha) and
 * ignored otherwise; `bitrate` (bits per second) applies to the H.264 and
 * H.265 encoders. NULL with a message in `err` on failure. */
atd_encoder *atd_encoder_open(const char *path, const char *name, int width, int height,
                              int fps_num, int fps_den, int profile, int64_t bitrate, char *err,
                              int errlen) {
    av_log_set_level(AV_LOG_FATAL);
    if (width <= 0 || height <= 0 || width % 2 || height % 2) {
        set_err(err, errlen, "frame width and height must be even and positive", 0);
        return NULL;
    }
    const AVCodec *codec = avcodec_find_encoder_by_name(name);
    if (!codec) {
        set_err(err, errlen, "this FFmpeg build has no such encoder", 0);
        return NULL;
    }
    atd_encoder *e = calloc(1, sizeof *e);
    if (!e) {
        set_err(err, errlen, "out of memory", 0);
        return NULL;
    }
    e->width = width;
    e->height = height;
    int r = avformat_alloc_output_context2(&e->fmt, NULL, NULL, path);
    if (r < 0 || !e->fmt) {
        set_err(err, errlen, "no container format for this file extension", r);
        goto fail;
    }
    if (avformat_query_codec(e->fmt->oformat, codec->id, FF_COMPLIANCE_NORMAL) != 1) {
        set_err(err, errlen, "this container cannot hold this codec (use .mov)", 0);
        goto fail;
    }
    e->st = avformat_new_stream(e->fmt, NULL);
    e->enc = avcodec_alloc_context3(codec);
    e->frame = av_frame_alloc();
    e->rgba = av_frame_alloc();
    e->pkt = av_packet_alloc();
    if (!e->st || !e->enc || !e->frame || !e->rgba || !e->pkt) {
        set_err(err, errlen, "out of memory", 0);
        goto fail;
    }
    AVCodecContext *c = e->enc;
    c->width = width;
    c->height = height;
    c->time_base = (AVRational){fps_den, fps_num};
    c->framerate = (AVRational){fps_num, fps_den};
    c->sample_aspect_ratio = (AVRational){1, 1};
    c->pix_fmt = choose_format(codec, c, profile >= 4);
    if (c->pix_fmt == AV_PIX_FMT_NONE) {
        set_err(err, errlen, "the encoder takes no software pixel format", 0);
        goto fail;
    }
    c->color_primaries = AVCOL_PRI_BT709;
    c->color_trc = AVCOL_TRC_BT709;
    c->colorspace = AVCOL_SPC_BT709;
    c->color_range = AVCOL_RANGE_MPEG;
    c->thread_count = 0;
    AVDictionary *opts = NULL;
    if (!strcmp(codec->name, "prores_ks")) {
        char p[8];
        snprintf(p, sizeof p, "%d", profile);
        av_dict_set(&opts, "profile", p, 0);
        if (profile >= 4) av_dict_set(&opts, "alpha_bits", "16", 0);
    } else {
        /* One keyframe a second, no B-frames: quick to seek and to decode. */
        c->gop_size = fps_den > 0 ? (fps_num + fps_den - 1) / fps_den : 30;
        c->max_b_frames = 0;
        if (bitrate > 0) c->bit_rate = bitrate;
    }
    if (e->fmt->oformat->flags & AVFMT_GLOBALHEADER) c->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    r = avcodec_open2(c, codec, &opts);
    av_dict_free(&opts);
    if (r < 0) {
        set_err(err, errlen, "opening the encoder (is the hardware encoder present?)", r);
        goto fail;
    }
    r = avcodec_parameters_from_context(e->st->codecpar, c);
    if (r < 0) {
        set_err(err, errlen, "stream parameters", r);
        goto fail;
    }
    e->st->time_base = c->time_base;
    e->st->avg_frame_rate = c->framerate;
    /* Apple players want HEVC tagged hvc1. */
    if (codec->id == AV_CODEC_ID_HEVC) e->st->codecpar->codec_tag = MKTAG('h', 'v', 'c', '1');

    e->frame->format = c->pix_fmt;
    e->frame->color_primaries = AVCOL_PRI_BT709;
    e->frame->color_trc = AVCOL_TRC_BT709;
    e->frame->colorspace = AVCOL_SPC_BT709;
    e->frame->color_range = AVCOL_RANGE_MPEG;
    e->rgba->format = AV_PIX_FMT_RGBA;
    e->rgba->width = width;
    e->rgba->height = height;
    e->rgba->linesize[0] = width * 4;
    e->rgba->color_primaries = AVCOL_PRI_BT709;
    e->rgba->color_trc = AVCOL_TRC_IEC61966_2_1;
    e->rgba->colorspace = AVCOL_SPC_RGB;
    e->rgba->color_range = AVCOL_RANGE_JPEG;
    e->frame->width = width;
    e->frame->height = height;
    r = av_frame_get_buffer(e->frame, 0);
    if (r < 0) {
        set_err(err, errlen, "frame buffer", r);
        goto fail;
    }
    /* RGBA in at full range; YUV out with BT.709 at limited range. FFmpeg 8
     * reads this from the frames' colour fields in sws_scale_frame (set
     * below; without them it converted RGBA to 10-bit 4:2:2 with the wrong
     * matrix); older versions use the details set here. */
    e->sws = sws_alloc_context();
    if (!e->sws) {
        set_err(err, errlen, "out of memory", 0);
        goto fail;
    }
    av_opt_set_int(e->sws, "srcw", width, 0);
    av_opt_set_int(e->sws, "srch", height, 0);
    av_opt_set_int(e->sws, "src_format", AV_PIX_FMT_RGBA, 0);
    av_opt_set_int(e->sws, "dstw", width, 0);
    av_opt_set_int(e->sws, "dsth", height, 0);
    av_opt_set_int(e->sws, "dst_format", c->pix_fmt, 0);
    av_opt_set_int(e->sws, "sws_flags", SWS_BICUBIC | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT, 0);
    sws_setColorspaceDetails(e->sws, sws_getCoefficients(SWS_CS_ITU709), 1,
                             sws_getCoefficients(SWS_CS_ITU709), 0, 0, 1 << 16, 1 << 16);
    r = sws_init_context(e->sws, NULL, NULL);
    if (r < 0) {
        set_err(err, errlen, "no conversion to the encoder's pixel format", r);
        goto fail;
    }

    if (!(e->fmt->oformat->flags & AVFMT_NOFILE)) {
        r = avio_open(&e->fmt->pb, path, AVIO_FLAG_WRITE);
        if (r < 0) {
            set_err(err, errlen, "creating the file", r);
            goto fail;
        }
    }
    r = avformat_write_header(e->fmt, NULL);
    if (r < 0) {
        set_err(err, errlen, "writing the file header", r);
        goto fail;
    }
    e->header_written = 1;
    return e;
fail:
    atd_encoder_free(e);
    return NULL;
}

/* Sends `frame` (NULL: flush) and writes every packet that comes out. */
static int drain(atd_encoder *e, AVFrame *frame, char *err, int errlen) {
    int r = avcodec_send_frame(e->enc, frame);
    if (r < 0) {
        set_err(err, errlen, "encoding", r);
        return -1;
    }
    for (;;) {
        r = avcodec_receive_packet(e->enc, e->pkt);
        if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) return 0;
        if (r < 0) {
            set_err(err, errlen, "encoding", r);
            return -1;
        }
        av_packet_rescale_ts(e->pkt, e->enc->time_base, e->st->time_base);
        e->pkt->stream_index = e->st->index;
        r = av_interleaved_write_frame(e->fmt, e->pkt);
        if (r < 0) {
            set_err(err, errlen, "writing", r);
            return -1;
        }
    }
}

/* Encodes one RGBA8 frame (width x height x 4 bytes, tightly packed). */
int atd_encoder_push(atd_encoder *e, const uint8_t *rgba, char *err, int errlen) {
    int r = av_frame_make_writable(e->frame);
    if (r < 0) {
        set_err(err, errlen, "frame buffer", r);
        return -1;
    }
    /* The frames carry their colour description, so swscale converts
     * full-range RGB to BT.709 limited-range YUV whichever path it takes. */
    e->rgba->data[0] = (uint8_t *)rgba;
    r = sws_scale_frame(e->sws, e->frame, e->rgba);
    e->rgba->data[0] = NULL;
    if (r < 0) {
        set_err(err, errlen, "converting the frame", r);
        return -1;
    }
    e->frame->pts = e->next_pts++;
    return drain(e, e->frame, err, errlen);
}

/* Flushes the encoder, writes the file trailer and frees the handle. */
int atd_encoder_finish(atd_encoder *e, char *err, int errlen) {
    int r = drain(e, NULL, err, errlen);
    if (r == 0) {
        int t = av_write_trailer(e->fmt);
        if (t < 0) {
            set_err(err, errlen, "finishing the file", t);
            r = -1;
        }
    }
    atd_encoder_free(e);
    return r;
}
