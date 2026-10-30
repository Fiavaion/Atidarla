// A plain Syphon app's client, using only Syphon.framework and Metal
// (Atidarla, MIT OR Apache-2.0): finds a server by name in the directory,
// copies every new frame to a CPU-visible texture and checks it against
// the stamped test pattern.
//
// peer_receive <name> <seconds>
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <Syphon/Syphon.h>
#include <stdatomic.h>
#include "peer_common.h"

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 3) {
            fprintf(stderr, "usage: peer_receive <name> <seconds>\n");
            return 2;
        }
        NSString *name = [NSString stringWithUTF8String:argv[1]];
        double seconds = atof(argv[2]);
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        id<MTLCommandQueue> queue = [device newCommandQueue];
        SyphonServerDirectory *directory = [SyphonServerDirectory sharedDirectory];

        __block atomic_ulong announced = 0;
        SyphonMetalClient *client = nil;
        id<MTLTexture> readable = nil;
        unsigned long copied = 0;
        uint8_t *pixels = NULL;
        unsigned long ok = 0, bad = 0, gaps = 0, resizes = 0;
        long last_frame = -1;
        int waiting = 0;
        NSDate *start = [NSDate date];
        while (-[start timeIntervalSinceNow] < seconds) {
            pump(0.001);
            if (client && !client.isValid) {
                printf("peer_receive: server gone\n");
                client = nil;
            }
            if (!client) {
                NSArray *found = [directory serversMatchingName:name appName:nil];
                if (found.count == 0) {
                    if (!waiting) printf("peer_receive: waiting for \"%s\"\n", argv[1]);
                    waiting = 1;
                    continue;
                }
                waiting = 0;
                atomic_store(&announced, 0);
                copied = 0;
                client = [[SyphonMetalClient alloc] initWithServerDescription:found.lastObject
                                                                       device:device
                                                                      options:nil
                                                              newFrameHandler:^(SyphonMetalClient *c) {
                                                                  atomic_fetch_add(&announced, 1);
                                                              }];
                printf("peer_receive: connected to \"%s\" (%s)\n", argv[1],
                       [found.lastObject[@"SyphonServerDescriptionAppNameKey"] UTF8String]);
            }
            unsigned long now = atomic_load(&announced);
            if (now == copied) continue;
            copied = now;
            id<MTLTexture> frame = [client newFrameImage];
            if (!frame) continue;
            NSUInteger w = frame.width, h = frame.height;
            if (!readable || readable.width != w || readable.height != h) {
                if (readable) resizes++;
                printf("peer_receive: %lux%lu pixel format %lu\n", (unsigned long)w, (unsigned long)h,
                       (unsigned long)frame.pixelFormat);
                MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:frame.pixelFormat
                                                                                             width:w
                                                                                            height:h
                                                                                         mipmapped:NO];
                d.storageMode = MTLStorageModeShared;
                readable = [device newTextureWithDescriptor:d];
                free(pixels);
                pixels = malloc(w * h * 4);
            }
            id<MTLCommandBuffer> cb = [queue commandBuffer];
            id<MTLBlitCommandEncoder> blit = [cb blitCommandEncoder];
            [blit copyFromTexture:frame toTexture:readable];
            [blit endEncoding];
            [cb commit];
            [cb waitUntilCompleted];
            [readable getBytes:pixels bytesPerRow:w * 4 fromRegion:MTLRegionMake2D(0, 0, w, h) mipmapLevel:0];
            Check c = check_pattern(pixels, (uint32_t)w, (uint32_t)h);
            if (!c.ok) {
                bad++;
                if (bad <= 5) printf("peer_receive: BAD frame %u: %zu pixels differ\n", c.frame, c.bad_bytes);
            } else if ((long)c.frame != last_frame) {
                ok++;
                if (last_frame >= 0 && (long)c.frame > last_frame + 1) gaps += c.frame - last_frame - 1;
                last_frame = c.frame;
            }
        }
        printf("peer_receive: verified %lu distinct frames, %lu bad, %lu skipped by number, %lu resizes\n", ok,
               bad, gaps, resizes);
        [client stop];
        free(pixels);
        return (bad > 0 || ok == 0) ? 1 : 0;
    }
}
