"""Entry point: `atd-ai-host --connect HOST:PORT` or `atd-ai-host --dev`."""

from __future__ import annotations

import argparse
import json
import logging
import socket
import sys

from .host import RpcError, Session

log = logging.getLogger("atd_ai_host")


def serve(conn: socket.socket) -> None:
    conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    session = Session()
    reader = conn.makefile("rb")
    try:
        for line in reader:
            request = json.loads(line)
            response: dict = {"jsonrpc": "2.0", "id": request.get("id")}
            try:
                response["result"] = session.handle(request["method"], request.get("params"))
            except RpcError as e:
                response["error"] = {"code": e.code, "message": e.message}
            except Exception as e:  # never let one bad request kill the host
                log.exception("request failed")
                response["error"] = {"code": -32603, "message": f"{type(e).__name__}: {e}"}
            conn.sendall(json.dumps(response, separators=(",", ":")).encode() + b"\n")
            if session.closed:
                break
    finally:
        if session.ring is not None:
            session.ring.close()
        reader.close()
        conn.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="atd-ai-host", description=__doc__)
    parser.add_argument("--connect", metavar="HOST:PORT", help="connect to an engine supervisor")
    parser.add_argument("--dev", action="store_true", help="listen on 127.0.0.1 and log verbosely")
    parser.add_argument("--port", type=int, default=0, help="port for --dev (default: any free port)")
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.DEBUG if args.dev else logging.WARNING, stream=sys.stderr)

    if args.connect:
        host, _, port = args.connect.rpartition(":")
        serve(socket.create_connection((host, int(port))))
        return 0
    if args.dev:
        with socket.create_server(("127.0.0.1", args.port)) as server:
            print(f"atd-ai-host listening on 127.0.0.1:{server.getsockname()[1]}", flush=True)
            while True:
                conn, peer = server.accept()
                log.info("session from %s", peer)
                serve(conn)
    parser.error("one of --connect or --dev is required")
    return 2


if __name__ == "__main__":
    sys.exit(main())
