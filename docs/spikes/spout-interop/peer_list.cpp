// Spout SDK sender-list peer (Atidarla interop check, MIT OR Apache-2.0).
// Built against the official Spout2 SDK (SpoutDX, BSD-2, Lynn Jarvis).
//
// usage: peer_list
//   Prints the SDK's view of the sender list, each sender's info and the
//   active sender.
#include "SpoutDX.h"
#include "peer_common.h"

int main() {
    spoutDX sdk;
    int count = sdk.GetSenderCount();
    printf("SDK list: %d sender(s), max %d\n", count, sdk.GetMaxSenders());
    for (int i = 0; i < count; i++) {
        char name[256] = {};
        if (!sdk.GetSender(i, name)) continue;
        unsigned w = 0, h = 0;
        HANDLE handle = nullptr;
        DWORD format = 0;
        if (sdk.GetSenderInfo(name, w, h, handle, format))
            printf("  \"%s\": %ux%u DXGI_FORMAT %lu handle 0x%llx\n", name, w, h, format,
                   (unsigned long long)(uintptr_t)handle);
        else
            printf("  \"%s\": no info map\n", name);
    }
    char active[256] = {};
    printf("SDK active sender: %s\n", sdk.GetActiveSender(active) ? active : "(none)");
    return 0;
}
