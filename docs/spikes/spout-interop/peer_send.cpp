// Spout SDK sender peer (Atidarla interop check, MIT OR Apache-2.0).
// Built against the official Spout2 SDK (SpoutDX, BSD-2, Lynn Jarvis).
//
// usage: peer_send <name> <seconds> <width> <height> [--rgba] [--resize] [--timing]
//   Sends the stamped test pattern (peer_common.h) with SpoutDX::SendTexture
//   at 60 fps, a new seed every frame. --timing: seed 0 uploaded once, then
//   only the 16-byte stamp per frame. --resize: halve the size half-way.
#include "SpoutDX.h"
#include "peer_common.h"

static ID3D11Texture2D* make_texture(ID3D11Device* dev, unsigned w, unsigned h, DXGI_FORMAT f) {
    D3D11_TEXTURE2D_DESC d = {};
    d.Width = w;
    d.Height = h;
    d.MipLevels = 1;
    d.ArraySize = 1;
    d.Format = f;
    d.SampleDesc.Count = 1;
    d.Usage = D3D11_USAGE_DEFAULT;
    d.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    ID3D11Texture2D* t = nullptr;
    if (FAILED(dev->CreateTexture2D(&d, nullptr, &t))) return nullptr;
    return t;
}

int main(int argc, char** argv) {
    if (argc < 5) {
        printf("usage: peer_send <name> <seconds> <width> <height> [--rgba] [--resize] [--timing]\n");
        return 2;
    }
    std::string name = argv[1];
    double seconds = atof(argv[2]);
    unsigned w = unsigned(atoi(argv[3])), h = unsigned(atoi(argv[4]));
    bool rgba = false, resize = false, timing = false;
    for (int i = 5; i < argc; i++) {
        rgba |= strcmp(argv[i], "--rgba") == 0;
        resize |= strcmp(argv[i], "--resize") == 0;
        timing |= strcmp(argv[i], "--timing") == 0;
    }
    DXGI_FORMAT fmt = rgba ? DXGI_FORMAT_R8G8B8A8_UNORM : DXGI_FORMAT_B8G8R8A8_UNORM;
    timeBeginPeriod(1);

    spoutDX tx;
    if (!tx.OpenDirectX11()) {
        printf("OpenDirectX11 failed\n");
        return 1;
    }
    char adapter[256] = {}, display[256] = {};
    tx.GetAdapterInfo(adapter, display, 256);
    printf("SDK sender, adapter: %s\n", adapter);
    ID3D11Device* dev = tx.GetDX11Device();
    ID3D11DeviceContext* ctx = tx.GetDX11Context();
    tx.SetSenderName(name.c_str());

    std::vector<uint8_t> buf(size_t(w) * h * 4);
    ID3D11Texture2D* tex = make_texture(dev, w, h, fmt);
    if (!tex) {
        printf("texture failed\n");
        return 1;
    }
    if (timing) {
        fill_pattern(buf.data(), w * 4, w, h, 0);
        ctx->UpdateSubresource(tex, 0, nullptr, buf.data(), w * 4, 0);
    }
    std::vector<double> send_ms;
    uint32_t frame = 0;
    bool resized = false;
    double start = qpc_ms();
    while (qpc_ms() - start < seconds * 1000.0) {
        if (resize && !resized && qpc_ms() - start > seconds * 500.0) {
            resized = true;
            w /= 2;
            h /= 2;
            printf("resizing to %ux%u\n", w, h);
            fflush(stdout);
            tex->Release();
            tex = make_texture(dev, w, h, fmt);
            buf.assign(size_t(w) * h * 4, 0);
            if (timing) {
                fill_pattern(buf.data(), w * 4, w, h, 0);
                ctx->UpdateSubresource(tex, 0, nullptr, buf.data(), w * 4, 0);
            }
        }
        if (timing) {
            uint8_t stamp[STAMP_LEN];
            write_stamp(stamp, frame, 0);
            D3D11_BOX box = {0, 0, 0, 4, 1, 1};
            ctx->UpdateSubresource(tex, 0, &box, stamp, STAMP_LEN, 0);
        } else {
            fill_pattern(buf.data(), w * 4, w, h, uint8_t(frame));
            write_stamp(buf.data(), frame, uint8_t(frame));
            ctx->UpdateSubresource(tex, 0, nullptr, buf.data(), w * 4, 0);
        }
        double t0 = qpc_ms();
        bool sent = tx.SendTexture(tex);
        send_ms.push_back(qpc_ms() - t0);
        if (frame == 0) {
            printf("SDK sender name: \"%s\" (%s) %ux%u DXGI_FORMAT %d\n", tx.GetName(),
                   sent ? "sent" : "send failed", w, h, int(fmt));
            fflush(stdout);
        }
        frame++;
        tx.HoldFps(60);
    }
    printf("sent %u frames; SendTexture ms mean / p50 / p95 / max: %s\n", frame,
           summary(send_ms).c_str());
    tex->Release();
    tx.ReleaseSender();
    return 0;
}
