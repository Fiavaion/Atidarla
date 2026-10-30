"""Connection modes: dial the engine (`--connect`) or accept sessions (`--listen`)."""

from __future__ import annotations

import ipaddress
import logging
import os
import socket
import sys
import threading
import time

from .session import HostContext, Session

log = logging.getLogger("atd_ai_host")


def split_addr(addr: str) -> tuple[str, int]:
    host, sep, port = addr.rpartition(":")
    if not sep or not port.isdigit():
        raise ValueError(f"expected HOST:PORT, got {addr!r}")
    return host.strip("[]") or "127.0.0.1", int(port)


def is_loopback(host: str) -> bool:
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False  # a hostname may resolve anywhere


def connect(addr: str, ctx: HostContext) -> None:
    """One session over a connection to the engine's supervisor."""
    Session(socket.create_connection(split_addr(addr)), ctx).run()


def listen(addr: str, ctx: HostContext) -> None:
    """Accepts sessions forever, one thread each."""
    host, port = split_addr(addr)
    family = socket.AF_INET6 if ":" in host else socket.AF_INET
    with socket.create_server((host, port), family=family) as server:
        bound = server.getsockname()
        shown = f"[{bound[0]}]" if family == socket.AF_INET6 else bound[0]
        print(f"atd-ai-host listening on {shown}:{bound[1]}", flush=True)
        while True:
            conn, peer = server.accept()
            log.info("session from %s", peer)
            threading.Thread(target=Session(conn, ctx).run, name=f"session:{peer}", daemon=True).start()


def pid_alive(pid: int) -> bool:
    if sys.platform == "win32":
        import ctypes

        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        code = ctypes.c_ulong()
        ok = kernel32.GetExitCodeProcess(handle, ctypes.byref(code))
        kernel32.CloseHandle(handle)
        return bool(ok) and code.value == 259  # STILL_ACTIVE
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def watch_parent(pid: int, interval: float = 1.0) -> None:
    """Exits the process once `pid` is gone, so a crashed engine leaves no orphan host."""
    was_parent = os.getppid() == pid

    def poll() -> None:
        while True:
            if not pid_alive(pid) or (was_parent and os.getppid() != pid):
                log.warning("parent process %s is gone; exiting", pid)
                os._exit(0)
            time.sleep(interval)

    threading.Thread(target=poll, name="parent-watch", daemon=True).start()
