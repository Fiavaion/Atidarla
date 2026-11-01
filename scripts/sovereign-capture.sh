#!/usr/bin/env bash
# Sovereign mode packet-capture hand check (PRD FR-A8, M4 AC5).
#
# Captures every packet while `lasair render` runs a project with Sovereign
# mode on (the default), then lists every remote address seen and fails if any
# is outside loopback, private (RFC 1918 / ULA), link-local or CGNAT ranges.
#
#   cargo build -p lsr-cli --release        # as yourself, first
#   sudo scripts/sovereign-capture.sh [project] [frames]
#
# project defaults to examples/ai-live.lasair, frames to 0-300. Set LASAIR
# to use another binary. The render runs as the user who called sudo.
#
# macOS captures on the PKTAP pseudo-interface, which tags each packet with
# the process that sent it: only Lasair's processes (lasair, lsr-*, the
# Python AI Host) are judged and other processes' traffic is listed as
# ignored. Linux cannot attribute packets, so every packet is judged: run it
# on a quiet machine.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project="${1:-$root/examples/ai-live.lasair}"
frames="${2:-0-300}"

if [[ "$(id -u)" -ne 0 ]]; then
    echo "run with sudo: tcpdump needs root to capture" >&2
    exit 2
fi
command -v tcpdump >/dev/null || { echo "tcpdump not found" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 not found (used to classify addresses)" >&2; exit 2; }

bin="${LASAIR:-}"
if [[ -z "$bin" ]]; then
    for b in "$root/target/release/lasair" "$root/target/debug/lasair"; do
        if [[ -x "$b" ]]; then bin="$b"; break; fi
    done
fi
if [[ -z "$bin" || ! -x "$bin" ]]; then
    echo "no lasair binary: run 'cargo build -p lsr-cli --release' (not as root) or set LASAIR" >&2
    exit 2
fi

user="${SUDO_USER:-$(id -un)}"
work="$(mktemp -d "${TMPDIR:-/tmp}/lsr-sovereign.XXXXXX")"
chown "$user" "$work"
log="$work/packets.txt"

case "$(uname -s)" in
    Darwin) capture=(tcpdump -n -l -k NP -i "pktap,all") ; attributed=1 ;;
    *)      capture=(tcpdump -n -l -i any) ; attributed=0 ;;
esac

"${capture[@]}" >"$log" 2>"$work/tcpdump.err" &
tcpdump_pid=$!
cleanup() { kill "$tcpdump_pid" 2>/dev/null || true; }
trap cleanup EXIT
sleep 2
if ! kill -0 "$tcpdump_pid" 2>/dev/null; then
    echo "tcpdump failed to start:" >&2
    cat "$work/tcpdump.err" >&2
    exit 2
fi

echo "capturing while: $bin render $project --range $frames (Sovereign mode on)"
status=0
sudo -u "$user" env -u LSR_SOVEREIGN "$bin" render "$project" --out "$work/frames" --range "$frames" || status=$?
sleep 2
cleanup
wait "$tcpdump_pid" 2>/dev/null || true
if [[ "$status" -ne 0 ]]; then
    echo "warning: the render exited with $status; judging the traffic it made anyway" >&2
fi

python3 - "$log" "$attributed" <<'PY'
import ipaddress
import re
import sys

path, attributed = sys.argv[1], sys.argv[2] == "1"
ours = re.compile(r"^(lasair|lsr-.*|python.*)$", re.IGNORECASE)
proc_re = re.compile(r"proc ([^:,()]+)")
# "IP 10.0.0.2.52000 > 1.2.3.4.443:" and "IP6 fe80::1.546 > ff02::1:2.547:"
flow_re = re.compile(r"\bIP6? (\S+) > (\S+?):? ")
# The ranges lsr_ai::egress counts as LAN. Not `is_private`: Python counts
# documentation ranges such as 203.0.113.0/24 as private.
lan = [
    ipaddress.ip_network(n)
    for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "100.64.0.0/10", "fc00::/7", "fe80::/10")
]


def addr(token: str):
    host = token.rsplit(".", 1)[0] if token.count(".") in (1, 4) or ":" in token else token
    try:
        return ipaddress.ip_address(host)
    except ValueError:
        try:
            return ipaddress.ip_address(token)
        except ValueError:
            return None


def local(ip) -> bool:
    if ip.version == 6 and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return ip.is_loopback or ip.is_multicast or ip.is_unspecified or any(ip in n for n in lan if n.version == ip.version)


seen: dict[str, set[str]] = {}
ignored: dict[str, set[str]] = {}
with open(path, errors="replace") as f:
    for line in f:
        m = flow_re.search(line)
        if not m:
            continue
        p = proc_re.search(line)
        proc = p.group(1).strip() if p else "?"
        for token in m.groups():
            ip = addr(token)
            if ip is None:
                continue
            judged = not attributed or ours.match(proc)
            (seen if judged else ignored).setdefault(str(ip), set()).add(proc)

bad = sorted(a for a in seen if not local(ipaddress.ip_address(a)))
print(f"remote addresses seen from {'Lasair processes' if attributed else 'this machine'}:")
for a in sorted(seen, key=lambda s: (ipaddress.ip_address(s).version, ipaddress.ip_address(s))):
    mark = "FAIL" if a in bad else "ok  "
    procs = ", ".join(sorted(seen[a]))
    print(f"  {mark} {a}" + (f"  ({procs})" if attributed else ""))
if ignored:
    print(f"other processes' traffic, not judged: {len(ignored)} addresses")
if bad:
    print(f"FAIL: {len(bad)} address(es) outside loopback/private/link-local/CGNAT/ULA")
    sys.exit(1)
print("PASS: no traffic left this machine and the LAN")
PY
