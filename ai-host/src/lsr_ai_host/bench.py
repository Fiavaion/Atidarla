"""`lsr-ai-host bench`: SD-Turbo img2img throughput (spike e, PT-3/PT-4/PT-5).

Direct mode times the adapter in this process. `--via-protocol` plays the
engine: it spawns a host with `--connect`, opens a session on a shared-memory
ring and times `frame.submit` round trips, which is what the engine sees.
"""

from __future__ import annotations

import argparse
import json
import platform
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from collections import deque
from pathlib import Path

from . import PROTOCOL_VERSION
from .frames import Frame
from .framing import encode_message, read_message
from .paths import resolve_model
from .ring import FrameRing, remove_quietly

PROMPT = "a watercolor painting of a harbour at sunset, soft light"


def input_frames(size: int, static: bool) -> list[Frame]:
    """A moving test pattern (8 frames that differ), or one frame repeated."""
    import numpy as np

    yy, xx = np.mgrid[0:size, 0:size]
    frames = []
    for i in range(1 if static else 8):
        shift = i * size // 8
        px = np.empty((size, size, 4), np.uint8)
        px[..., 0] = ((xx + shift) * 255 // size) % 256
        px[..., 1] = (yy * 255 // size) % 256
        px[..., 2] = (((xx // 32) + (yy // 32) + i) % 2) * 200
        px[..., 3] = 255
        frames.append(Frame(size, size, px.tobytes()))
    return frames


def params_of(args) -> dict:
    return {
        "prompts": [{"text": args.prompt, "weight": 1.0}],
        "seed": 1,
        "strength": args.strength,
        "steps": args.steps,
        "guidance": args.guidance,
        "rcfg": args.rcfg,
        "delta": 1.0,
        "similarity": args.similarity,
    }


def summarise(times_ms: list[float], wall_s: float, skipped: int, extra: dict) -> dict:
    q = statistics.quantiles(times_ms, n=100) if len(times_ms) > 1 else times_ms * 99
    return {
        "frames": len(times_ms),
        "fps": round(len(times_ms) / wall_s, 1),
        "p50_ms": round(statistics.median(times_ms), 2),
        "p95_ms": round(q[94], 2),
        "max_ms": round(max(times_ms), 2),
        "skipped": skipped,
        **extra,
    }


def peak_rss_mb() -> int | None:
    try:
        import resource
    except ImportError:
        return None
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return rss // (1 << 20) if sys.platform == "darwin" else rss // 1024


def run_direct(args) -> dict:
    import torch

    from .adapters.diffusion import DiffusionAdapter

    t0 = time.perf_counter()
    sd = DiffusionAdapter("bench", str(args.model), {}, lambda stage, fraction: None)
    load_ms = (time.perf_counter() - t0) * 1e3
    state = sd.new_state()
    frames = input_frames(args.size, args.static)
    params = params_of(args)
    for i in range(args.warmup):
        sd.process(state, frames[i % len(frames)], params)
    if sd.device == "cuda":
        torch.cuda.reset_peak_memory_stats()
    gpu_peak = 0
    times, skipped = [], 0
    start = time.perf_counter()
    for i in range(args.frames):
        t = time.perf_counter()
        _, was_skipped = sd.process(state, frames[i % len(frames)], params)
        times.append((time.perf_counter() - t) * 1e3)
        skipped += was_skipped
        if sd.device == "mps" and i % 10 == 0:
            gpu_peak = max(gpu_peak, torch.mps.driver_allocated_memory())
    wall = time.perf_counter() - start
    if sd.device == "cuda":
        gpu_peak = torch.cuda.max_memory_allocated()
    return summarise(
        times,
        wall,
        skipped,
        {
            "device": sd.device,
            "dtype": str(sd.dtype).removeprefix("torch."),
            "vae": "taesd" if sd.tiny_vae else "full",
            "load_ms": round(load_ms),
            "gpu_peak_mb": gpu_peak // (1 << 20) if gpu_peak else None,
            "rss_peak_mb": peak_rss_mb(),
        },
    )


class _Client:
    def __init__(self, sock: socket.socket) -> None:
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.settimeout(120)
        self.sock = sock
        self.reader = sock.makefile("rb")
        self.next_id = 1
        self.inbox: dict[int, dict] = {}

    def send(self, method: str, params: dict | None = None) -> int:
        rid = self.next_id
        self.next_id += 1
        for chunk in encode_message({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}):
            self.sock.sendall(chunk)
        return rid

    def result(self, rid: int):
        while rid not in self.inbox:
            got = read_message(self.reader)
            if got is None:
                raise ConnectionError("host closed the connection")
            if got[0].get("id") is not None:
                self.inbox[got[0]["id"]] = got[0]
        msg = self.inbox.pop(rid)
        if "error" in msg:
            raise RuntimeError(f"host error {msg['error']['code']}: {msg['error']['message']}")
        return msg["result"]

    def call(self, method: str, params: dict | None = None):
        return self.result(self.send(method, params))


def run_via_protocol(args) -> dict:
    size, depth = args.size, args.inflight
    ring_path = str(Path(tempfile.gettempdir()) / f"lsr-bench-ring-{time.time_ns()}")
    ring = FrameRing.create(ring_path, 2 * depth, size, size)
    server = socket.create_server(("127.0.0.1", 0))
    host = subprocess.Popen([sys.executable, "-m", "lsr_ai_host", "--connect", f"127.0.0.1:{server.getsockname()[1]}"])
    try:
        server.settimeout(60)
        conn, _ = server.accept()
        c = _Client(conn)
        c.call(
            "session.open",
            {
                "protocol_version": PROTOCOL_VERSION,
                "client": "lsr-ai-host bench",
                "transport": {"kind": "shm", "ring_path": ring_path},
                "token": None,
            },
        )
        t0 = time.perf_counter()
        info = c.call(
            "model.load", {"model_id": "bench", "adapter": "diffusion.sd", "path": str(args.model), "options": {}}
        )
        load_ms = (time.perf_counter() - t0) * 1e3
        frames = input_frames(size, args.static)
        params = params_of(args)
        times: list[float] = []
        host_ms: list[float] = []
        skipped = 0
        gpu_peak = 0
        pending: deque = deque()

        def submit(seq: int) -> None:
            k = seq % depth
            ring.write(2 * k, seq, frames[seq % len(frames)].data)
            request = {
                "seq": seq,
                "timestamp_us": time.monotonic_ns() // 1000,
                "model_id": "bench",
                "width": size,
                "height": size,
                "input_slot": 2 * k,
                "output_slot": 2 * k + 1,
                "aux": {},
                "params": params,
            }
            pending.append((c.send("frame.submit", request), time.perf_counter()))

        def finish(record: bool) -> None:
            nonlocal skipped
            rid, sent = pending.popleft()
            result = c.result(rid)
            if record:
                times.append((time.perf_counter() - sent) * 1e3)
                host_ms.append(result["host_ms"])
                skipped += result["skipped"]

        total = args.warmup + args.frames
        start = None
        for seq in range(total):
            if seq == args.warmup:
                while pending:
                    finish(False)
                start = time.perf_counter()
            submit(seq)
            if len(pending) >= depth:
                finish(seq >= args.warmup)
            if seq % 30 == 0 and seq >= args.warmup:
                gpu_peak = max(gpu_peak, c.call("host.health").get("gpu_mem_mb") or 0)
        while pending:
            finish(True)
        wall = time.perf_counter() - start
        c.call("session.close")
        out = summarise(
            times,
            wall,
            skipped,
            {
                "device": info["device"],
                "inflight": depth,
                "host_p50_ms": round(statistics.median(host_ms), 2),
                "load_ms": round(load_ms),
                "gpu_peak_mb": gpu_peak or None,
            },
        )
        host.wait(timeout=30)
        return out
    finally:
        host.kill()
        server.close()
        ring.close()
        remove_quietly(ring_path)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="lsr-ai-host bench", description=__doc__)
    parser.add_argument("--model", default="sd-turbo", help="model folder, or a name in the model cache")
    parser.add_argument("--size", type=int, default=512)
    parser.add_argument("--steps", type=int, default=1, choices=[1, 2, 3, 4])
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--warmup", type=int, default=30)
    parser.add_argument("--strength", type=float, default=0.5)
    parser.add_argument("--guidance", type=float, default=1.0, help="> 1 enables CFG / R-CFG")
    parser.add_argument("--rcfg", choices=["none", "self", "onetime"], default="none")
    parser.add_argument("--similarity", type=float, default=0.0, help="SSF threshold eta (0 = off)")
    parser.add_argument("--static", action="store_true", help="feed one unchanging frame (shows SSF)")
    parser.add_argument("--prompt", default=PROMPT)
    parser.add_argument("--via-protocol", action="store_true", help="through a --connect host and the shm ring")
    parser.add_argument("--inflight", type=int, default=1, help="frames in flight with --via-protocol")
    args = parser.parse_args(argv)
    args.model = resolve_model(args.model)
    if not (args.model / "unet").is_dir():
        parser.error(f"{args.model} is not a diffusers model folder")
    if args.rcfg != "none" and args.guidance <= 1.0:
        args.guidance = 1.5  # R-CFG needs guidance > 1 to do anything

    result = run_via_protocol(args) if args.via_protocol else run_direct(args)
    import torch

    config = {
        "mode": "protocol" if args.via_protocol else "direct",
        "size": args.size,
        "steps": args.steps,
        "rcfg": args.rcfg if args.guidance > 1.0 else "none",
        "guidance": args.guidance,
        "similarity": args.similarity,
        "static": args.static,
        "machine": f"{platform.machine()} {platform.system()} {platform.release()}",
        "python": platform.python_version(),
        "torch": torch.__version__,
    }
    print(json.dumps({**config, **result}))
    return 0
