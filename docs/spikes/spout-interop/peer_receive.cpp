// Spout SDK receiver peer (Lasair interop check, MIT OR Apache-2.0).
// Built against the official Spout2 SDK (SpoutDX, BSD-2, Lynn Jarvis).
//
// usage: peer_receive <sender name | -> <seconds> [--checksum]
//   "-" follows the active sender. Every new frame is copied by the SDK's
//   ReceiveTexture(), read back through a staging texture and checked
//   against the stamped test pattern (peer_common.h). With --checksum the
//   pattern check is replaced by an FNV-1a checksum print every 0.5 s (for
//   senders that do not send the pattern).
#include "SpoutDX.h"
#include "peer_common.h"

#include <set>

int main(int argc, char** argv) {
    if (argc < 3) {
        printf("usage: peer_receive <sender name | -> <seconds> [--checksum]\n");
        return 2;
    }
    std::string name = argv[1];
    double seconds = atof(argv[2]);
    bool checksum = argc > 3 && strcmp(argv[3], "--checksum") == 0;
    timeBeginPeriod(1);

    spoutDX rx;
    if (!rx.OpenDirectX11()) {
        printf("OpenDirectX11 failed\n");
        return 1;
    }
    char adapter[256] = {}, display[256] = {};
    rx.GetAdapterInfo(adapter, display, 256);
    printf("SDK receiver, adapter: %s\n", adapter);
    int count = rx.GetSenderCount();
    printf("SDK sees %d sender(s):", count);
    for (int i = 0; i < count; i++) {
        char s[256] = {};
        if (rx.GetSender(i, s)) printf(" \"%s\"", s);
    }
    char active[256] = {};
    printf("\nSDK active sender: %s\n", rx.GetActiveSender(active) ? active : "(none)");
    if (name != "-") rx.SetReceiverName(name.c_str());

    ID3D11Device* dev = rx.GetDX11Device();
    ID3D11DeviceContext* ctx = rx.GetDX11Context();
    ID3D11Texture2D* staging = nullptr;
    unsigned w = 0, h = 0;
    std::set<uint32_t> distinct;
    uint32_t last_frame = UINT32_MAX;
    size_t copies = 0, ok = 0, bad = 0, gaps = 0;
    std::vector<double> receive_ms, age_ms, readback_ms;
    bool connected = false;
    double start = qpc_ms(), last_print = 0;
    while (qpc_ms() - start < seconds * 1000.0) {
        double t0 = qpc_ms();
        bool got = rx.ReceiveTexture();
        double t1 = qpc_ms();
        if (!got) {
            if (connected) printf("sender gone\n");
            connected = false;
            Sleep(1);
            continue;
        }
        if (rx.IsUpdated()) {
            connected = true;
            w = rx.GetSenderWidth();
            h = rx.GetSenderHeight();
            printf("connected to \"%s\": %ux%u DXGI_FORMAT %d\n", rx.GetSenderName(), w, h,
                   int(rx.GetSenderFormat()));
            if (staging) staging->Release();
            D3D11_TEXTURE2D_DESC d = {};
            d.Width = w;
            d.Height = h;
            d.MipLevels = 1;
            d.ArraySize = 1;
            d.Format = rx.GetSenderFormat();
            d.SampleDesc.Count = 1;
            d.Usage = D3D11_USAGE_STAGING;
            d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
            if (FAILED(dev->CreateTexture2D(&d, nullptr, &staging))) {
                printf("staging texture failed\n");
                return 1;
            }
            continue;
        }
        if (!rx.IsFrameNew() || !staging) {
            Sleep(1);
            continue;
        }
        ID3D11Texture2D* tex = rx.GetSenderTexture();
        if (!tex) continue;
        copies++;
        double r0 = qpc_ms();
        ctx->CopyResource(staging, tex);
        D3D11_MAPPED_SUBRESOURCE m = {};
        if (FAILED(ctx->Map(staging, 0, D3D11_MAP_READ, 0, &m))) {
            printf("Map failed\n");
            continue;
        }
        double mapped_ms = qpc_ms() - r0;
        const uint8_t* px = static_cast<const uint8_t*>(m.pData);
        if (checksum) {
            if (qpc_ms() - last_print > 500) {
                last_print = qpc_ms();
                uint32_t hsh = 0x811c9dc5u;
                for (unsigned y = 0; y < h; y++)
                    for (unsigned b = 0; b < w * 4; b++)
                        hsh = (hsh ^ px[size_t(y) * m.RowPitch + b]) * 0x01000193u;
                printf("frame: %ux%u checksum %08x\n", w, h, hsh);
            }
            ctx->Unmap(staging, 0);
            Sleep(1);
            continue;
        }
        Check c = check_pattern(px, m.RowPitch, w, h);
        ctx->Unmap(staging, 0);
        readback_ms.push_back(mapped_ms);
        if (!c.ok) {
            if (++bad <= 5) printf("BAD frame %u: %zu pixels differ\n", c.frame, c.bad_bytes);
        } else if (c.frame != last_frame) {
            ok++;
            receive_ms.push_back(t1 - t0);
            age_ms.push_back(c.age_ms);
            if (last_frame != UINT32_MAX && c.frame > last_frame + 1) gaps += c.frame - last_frame - 1;
            last_frame = c.frame;
            distinct.insert(c.frame);
        }
        Sleep(1);
    }
    printf("copies %zu, verified: %zu distinct frames match, %zu bad, %zu skipped by number\n",
           copies, ok, bad, gaps);
    printf("ReceiveTexture ms mean / p50 / p95 / max: %s\n", summary(receive_ms).c_str());
    printf("stamp-to-pixels-on-CPU ms (includes readback): %s\n", summary(age_ms).c_str());
    printf("readback ms (copy to staging + map): %s\n", summary(readback_ms).c_str());
    if (staging) staging->Release();
    rx.ReleaseReceiver();
    return 0;
}
