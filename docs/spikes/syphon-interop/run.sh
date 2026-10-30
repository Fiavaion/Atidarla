#!/bin/bash
# Syphon interop check (macOS): Atidarla's Syphon side against plain
# Syphon.framework peers in other processes, both directions, with a resize
# mid-stream. See docs/spikes/a-texture-sharing.md.
#
#   docs/spikes/syphon-interop/run.sh [out-dir]
#
# Needs Xcode (clang, and the framework that `cargo build` makes for
# atd-interop). Peers are built into out-dir (default .scratch/syphon-peers).
set -euo pipefail
cd "$(dirname "$0")/../../.."
out="${1:-.scratch/syphon-peers}"
mkdir -p "$out"

cargo build -q --release -p atd-interop --example syphon_send --example syphon_receive
framework_dir="$(dirname "$(ls -td target/release/build/atd-interop-*/out/syphon-build/Release/Syphon.framework | head -1)")"
echo "framework: $framework_dir/Syphon.framework"
for peer in peer_send peer_receive; do
    clang -fobjc-arc -O2 -F"$framework_dir" -framework Syphon -framework Foundation -framework Metal \
        -Wl,-rpath,"$PWD/$framework_dir" "docs/spikes/syphon-interop/$peer.m" -o "$out/$peer"
done
send=target/release/examples/syphon_send
receive=target/release/examples/syphon_receive
status=0

case_run() { # title, sender command..., --, receiver command...
    local title="$1"; shift
    local sender=() receiver=()
    while [ "$1" != "--" ]; do sender+=("$1"); shift; done; shift
    receiver=("$@")
    echo "== $title"
    "${sender[@]}" > "$out/sender.log" 2>&1 &
    local pid=$!
    sleep 1
    "${receiver[@]}" || status=1
    wait "$pid" || status=1
    sed 's/^/   sender: /' "$out/sender.log"
}

# Server first, client second: the client finds a running server (the
# directory's announce request, answered on the server's main run loop).
case_run "ours -> Syphon peer, BGRA8 1920x1080, resized to 960x540" \
    "$send" atd-interop-a 8 1920 1080 --resize -- "$out/peer_receive" atd-interop-a 8
case_run "Syphon peer -> ours, BGRA8 1920x1080, resized to 960x540" \
    "$out/peer_send" atd-interop-b 8 1920 1080 --resize -- "$receive" atd-interop-b 8

# Client first, server second: the server's announcement on start.
echo "== ours -> ours (client waiting first), 1280x720"
"$receive" atd-interop-c 6 > "$out/receiver.log" 2>&1 &
pid=$!
sleep 1
"$send" atd-interop-c 4 1280 720 || status=1
wait "$pid" || status=1
sed 's/^/   receiver: /' "$out/receiver.log"

exit $status
