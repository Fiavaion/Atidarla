"""lsr-ai-host: run model work out of process for the Lasair engine.

lsr-ai-host --connect HOST:PORT                  dial the engine's supervisor (one session)
lsr-ai-host --listen ADDR:PORT [--token T]       accept sessions (LAN, cloud, or local engine)
lsr-ai-host --dev                                listen on 127.0.0.1 and log verbosely
lsr-ai-host bench --model DIR ...                SD-Turbo benchmark (see `bench --help`)
"""

from __future__ import annotations

import argparse
import logging
import os
import sys

from . import capabilities
from .server import connect, is_loopback, listen, split_addr, watch_parent
from .session import HostContext, HostLogHandler


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["bench"]:
        from .bench import main as bench_main

        return bench_main(argv[1:])

    parser = argparse.ArgumentParser(
        prog="lsr-ai-host", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--connect", metavar="HOST:PORT", help="connect to an engine supervisor")
    mode.add_argument("--listen", metavar="ADDR:PORT", help="accept sessions on this address (port 0 = any)")
    mode.add_argument("--dev", action="store_true", help="listen on 127.0.0.1 and log verbosely")
    parser.add_argument("--port", type=int, default=0, help="port for --dev (default: any free port)")
    parser.add_argument("--token", help="session token for --listen (default: $LSR_AI_HOST_TOKEN)")
    parser.add_argument("--parent-pid", type=int, metavar="PID", help="exit when this process is gone")
    parser.add_argument("--verbose", "-v", action="store_true", help="log at debug level")
    # Tests only: report the capabilities of another vendor's GPU box (routing tests on loopback).
    parser.add_argument("--fake-vendor", choices=["nvidia", "amd", "intel", "apple"], help=argparse.SUPPRESS)
    args = parser.parse_args(argv)

    logging.basicConfig(level=logging.DEBUG if args.dev or args.verbose else logging.INFO, stream=sys.stderr)
    capabilities.FAKE_VENDOR = args.fake_vendor
    token = args.token or os.environ.get("LSR_AI_HOST_TOKEN") or None
    if args.parent_pid is not None:
        watch_parent(args.parent_pid)

    if args.connect:
        ctx = HostContext(None)
        logging.getLogger("lsr_ai_host").addHandler(HostLogHandler(ctx))
        connect(args.connect, ctx)
        return 0
    if args.listen or args.dev:
        addr = args.listen or f"127.0.0.1:{args.port}"
        try:
            host, _ = split_addr(addr)
        except ValueError as e:
            parser.error(str(e))
        if not is_loopback(host) and not token:
            parser.error(f"refusing to listen on {host} without --token (or LSR_AI_HOST_TOKEN)")
        ctx = HostContext(token)
        logging.getLogger("lsr_ai_host").addHandler(HostLogHandler(ctx))
        try:
            listen(addr, ctx)
        except KeyboardInterrupt:
            return 0
        return 0
    parser.error("one of --connect, --listen or --dev is required")
    return 2


if __name__ == "__main__":
    sys.exit(main())
