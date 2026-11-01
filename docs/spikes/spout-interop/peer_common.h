// Shared by the Spout SDK interop peers (Lasair, MIT OR Apache-2.0).
// Mirrors lsr_interop::readback::test_pattern and
// crates/lsr-interop/examples/common/stamped.rs byte for byte.
#pragma once

#include <windows.h>
#include <d3d11.h>
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

static const size_t STAMP_LEN = 16;

// FILETIME ticks (100 ns since 1601), comparable across processes.
inline uint64_t now_ticks() {
    FILETIME ft;
    GetSystemTimePreciseAsFileTime(&ft);
    return (uint64_t(ft.dwHighDateTime) << 32) | ft.dwLowDateTime;
}

// Pixel i of test_pattern: x = (i % w) as u8, y = (i / w) as u8,
// [x * 7 + seed, y * 13, x ^ y, 255].
inline void fill_pattern(uint8_t* dst, uint32_t pitch, uint32_t w, uint32_t h, uint8_t seed) {
    for (uint32_t y = 0; y < h; y++) {
        uint8_t* row = dst + size_t(y) * pitch;
        for (uint32_t x = 0; x < w; x++) {
            uint8_t xb = uint8_t(x), yb = uint8_t(y);
            row[x * 4 + 0] = uint8_t(xb * 7 + seed);
            row[x * 4 + 1] = uint8_t(yb * 13);
            row[x * 4 + 2] = uint8_t(xb ^ yb);
            row[x * 4 + 3] = 255;
        }
    }
}

inline void write_stamp(uint8_t* row0, uint32_t frame, uint32_t seed) {
    uint64_t t = now_ticks();
    memcpy(row0, &frame, 4);
    memcpy(row0 + 4, &seed, 4);
    memcpy(row0 + 8, &t, 8);
}

struct Check {
    bool ok;
    uint32_t frame, seed;
    double age_ms;
    size_t bad_bytes;
};

// Reads the stamp and compares every byte after it with the pattern.
inline Check check_pattern(const uint8_t* src, uint32_t pitch, uint32_t w, uint32_t h) {
    Check c{};
    memcpy(&c.frame, src, 4);
    memcpy(&c.seed, src + 4, 4);
    uint64_t sent;
    memcpy(&sent, src + 8, 8);
    c.age_ms = (double(now_ticks()) - double(sent)) / 1e4;
    if (c.seed > 255) {
        c.bad_bytes = 1;
        return c;
    }
    for (uint32_t y = 0; y < h; y++) {
        const uint8_t* row = src + size_t(y) * pitch;
        for (uint32_t x = (y == 0 ? 4 : 0); x < w; x++) {
            uint8_t xb = uint8_t(x), yb = uint8_t(y);
            uint8_t e[4] = {uint8_t(xb * 7 + c.seed), uint8_t(yb * 13), uint8_t(xb ^ yb), 255};
            if (memcmp(row + x * 4, e, 4) != 0) c.bad_bytes++;
        }
    }
    c.ok = c.bad_bytes == 0;
    return c;
}

inline double qpc_ms() {
    static LARGE_INTEGER f = [] { LARGE_INTEGER x; QueryPerformanceFrequency(&x); return x; }();
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return double(t.QuadPart) * 1e3 / double(f.QuadPart);
}

// "mean / p50 / p95 / max" in ms.
inline std::string summary(std::vector<double> v) {
    if (v.empty()) return "n/a";
    std::sort(v.begin(), v.end());
    double mean = 0;
    for (double x : v) mean += x;
    mean /= double(v.size());
    char buf[128];
    snprintf(buf, sizeof buf, "%.3f / %.3f / %.3f / %.3f", mean, v[v.size() / 2],
             v[v.size() * 95 / 100], v.back());
    return buf;
}
