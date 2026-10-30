// Shared by the Syphon interop peers (Atidarla, MIT OR Apache-2.0).
// Mirrors atd_interop::readback::test_pattern and
// crates/atd-interop/examples/common/stamped.rs byte for byte.
#pragma once

#import <Foundation/Foundation.h>
#include <stdint.h>
#include <string.h>
#include <sys/time.h>

static const size_t STAMP_LEN = 16;

// Windows FILETIME ticks (100 ns since 1601), as stamped.rs uses.
static inline uint64_t now_ticks(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return 116444736000000000ull + (uint64_t)ts.tv_sec * 10000000ull + (uint64_t)ts.tv_nsec / 100;
}

// Pixel i of test_pattern: x = (i % w) as u8, y = (i / w) as u8,
// [x * 7 + seed, y * 13, x ^ y, 255]; tightly packed rows.
static inline void fill_pattern(uint8_t *dst, uint32_t w, uint32_t h, uint8_t seed) {
    for (uint32_t y = 0; y < h; y++) {
        uint8_t *row = dst + (size_t)y * w * 4;
        for (uint32_t x = 0; x < w; x++) {
            uint8_t xb = (uint8_t)x, yb = (uint8_t)y;
            row[x * 4 + 0] = (uint8_t)(xb * 7 + seed);
            row[x * 4 + 1] = (uint8_t)(yb * 13);
            row[x * 4 + 2] = (uint8_t)(xb ^ yb);
            row[x * 4 + 3] = 255;
        }
    }
}

static inline void write_stamp(uint8_t *px, uint32_t frame, uint32_t seed) {
    uint64_t t = now_ticks();
    memcpy(px, &frame, 4);
    memcpy(px + 4, &seed, 4);
    memcpy(px + 8, &t, 8);
}

typedef struct {
    int ok;
    uint32_t frame, seed;
    double age_ms;
    size_t bad_bytes;
} Check;

// Reads the stamp and compares every byte after it with the pattern.
static inline Check check_pattern(const uint8_t *px, uint32_t w, uint32_t h) {
    Check c = {0};
    memcpy(&c.frame, px, 4);
    memcpy(&c.seed, px + 4, 4);
    uint64_t sent;
    memcpy(&sent, px + 8, 8);
    c.age_ms = ((double)now_ticks() - (double)sent) / 1e4;
    if (c.seed > 255) {
        c.bad_bytes = 1;
        return c;
    }
    for (uint32_t y = 0; y < h; y++) {
        const uint8_t *row = px + (size_t)y * w * 4;
        for (uint32_t x = (y == 0 ? 4 : 0); x < w; x++) {
            uint8_t xb = (uint8_t)x, yb = (uint8_t)y;
            uint8_t e[4] = {(uint8_t)(xb * 7 + c.seed), (uint8_t)(yb * 13), (uint8_t)(xb ^ yb), 255};
            if (memcmp(row + x * 4, e, 4) != 0) c.bad_bytes++;
        }
    }
    c.ok = c.bad_bytes == 0;
    return c;
}

// Runs the main run loop for `seconds`: Syphon's directory and servers
// hear each other through distributed notifications delivered there.
static inline void pump(double seconds) {
    [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                             beforeDate:[NSDate dateWithTimeIntervalSinceNow:seconds]];
}
