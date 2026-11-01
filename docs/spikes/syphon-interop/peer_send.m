// A plain Syphon app's server, using only Syphon.framework and Metal
// (Lasair, MIT OR Apache-2.0): publishes the stamped test pattern at 60
// fps, a new seed every frame, from a Metal texture filled on the CPU.
//
// peer_send <name> <seconds> <width> <height> [--resize]
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <Syphon/Syphon.h>
#include "peer_common.h"

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 5) {
            fprintf(stderr, "usage: peer_send <name> <seconds> <width> <height> [--resize]\n");
            return 2;
        }
        NSString *name = [NSString stringWithUTF8String:argv[1]];
        double seconds = atof(argv[2]);
        uint32_t w = (uint32_t)atoi(argv[3]), h = (uint32_t)atoi(argv[4]);
        int resize = argc > 5 && strcmp(argv[5], "--resize") == 0;

        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        id<MTLCommandQueue> queue = [device newCommandQueue];
        SyphonMetalServer *server = [[SyphonMetalServer alloc] initWithName:name device:device options:nil];
        if (!server) {
            fprintf(stderr, "peer_send: server did not start\n");
            return 1;
        }
        printf("peer_send: server \"%s\" %ux%u\n", argv[1], w, h);
        fflush(stdout);

        id<MTLTexture> texture = nil;
        uint32_t tw = 0, th = 0;
        uint8_t *pixels = NULL;
        NSDate *start = [NSDate date];
        uint32_t frame = 0;
        while (-[start timeIntervalSinceNow] < seconds) {
            if (resize && -[start timeIntervalSinceNow] > seconds / 2) {
                resize = 0;
                w /= 2;
                h /= 2;
                printf("peer_send: resized to %ux%u\n", w, h);
                fflush(stdout);
            }
            if (tw != w || th != h) {
                MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                             width:w
                                                                                            height:h
                                                                                         mipmapped:NO];
                d.usage = MTLTextureUsageShaderRead;
                d.storageMode = MTLStorageModeShared;
                texture = [device newTextureWithDescriptor:d];
                free(pixels);
                pixels = malloc((size_t)w * h * 4);
                tw = w;
                th = h;
            }
            fill_pattern(pixels, w, h, (uint8_t)(frame % 256));
            write_stamp(pixels, frame, frame % 256);
            [texture replaceRegion:MTLRegionMake2D(0, 0, w, h) mipmapLevel:0 withBytes:pixels bytesPerRow:w * 4];
            id<MTLCommandBuffer> cb = [queue commandBuffer];
            [server publishFrameTexture:texture onCommandBuffer:cb imageRegion:NSMakeRect(0, 0, w, h) flipped:NO];
            [cb commit];
            frame++;
            double due = frame / 60.0 + [start timeIntervalSinceNow];
            if (due > 0) pump(due);
        }
        printf("peer_send: sent %u frames\n", frame);
        [server stop];
        free(pixels);
    }
    return 0;
}
