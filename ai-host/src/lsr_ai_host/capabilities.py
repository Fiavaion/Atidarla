"""What this host can run (`host.capabilities`), for the engine's compute router.

ML runtimes are imported only when installed, and the result is computed once
per process because importing torch takes a second or two.
"""

from __future__ import annotations

import functools
import importlib.util
import platform
import shutil
import subprocess
import sys

from .adapters import available_kinds
from .adapters.builtin import BUILTIN_MODELS

_ORT_EP = {
    "TensorrtExecutionProvider": "tensorrt",
    "CUDAExecutionProvider": "cuda",
    "DmlExecutionProvider": "directml",
    "CoreMLExecutionProvider": "coreml",
    "ROCMExecutionProvider": "rocm",
    "OpenVINOExecutionProvider": "openvino",
    "CPUExecutionProvider": "cpu",
}

# Set by `--fake-vendor` (tests only): changes what is reported, nothing else.
FAKE_VENDOR: str | None = None


def installed(module: str) -> bool:
    return importlib.util.find_spec(module) is not None


def _sysctl(name: str) -> str | None:
    try:
        return (
            subprocess.run(
                ["sysctl", "-n", name], capture_output=True, text=True, timeout=2, check=False
            ).stdout.strip()
            or None
        )
    except (OSError, subprocess.SubprocessError):
        return None


def _nvidia_smi() -> list[dict]:
    exe = shutil.which("nvidia-smi")
    if exe is None:
        return []
    try:
        out = subprocess.run(
            [exe, "--query-gpu=name,memory.total", "--format=csv,noheader,nounits"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    devices = []
    for line in out.splitlines():
        name, _, mem = line.rpartition(",")
        if name:
            devices.append(
                {"name": name.strip(), "vendor": "nvidia", "vram_mb": int(mem) if mem.strip().isdigit() else None}
            )
    return devices


def _torch_devices(runtimes: list[str]) -> list[dict]:
    import torch

    devices = []
    if torch.cuda.is_available():
        runtimes.append("torch-cuda")
        vendor = "amd" if getattr(torch.version, "hip", None) else "nvidia"
        for i in range(torch.cuda.device_count()):
            props = torch.cuda.get_device_properties(i)
            devices.append({"name": props.name, "vendor": vendor, "vram_mb": props.total_memory // (1 << 20)})
    if torch.backends.mps.is_available():
        runtimes.append("torch-mps")
        vram = None
        if hasattr(torch.mps, "recommended_max_memory"):
            vram = torch.mps.recommended_max_memory() // (1 << 20)
        devices.append({"name": _sysctl("machdep.cpu.brand_string") or "Apple GPU", "vendor": "apple", "vram_mb": vram})
    return devices


@functools.cache
def detect() -> dict:
    runtimes: list[str] = []
    devices: list[dict] = []
    if installed("torch"):
        devices = _torch_devices(runtimes)
    if not devices:
        devices = _nvidia_smi()
    if not devices and sys.platform == "darwin" and platform.machine() == "arm64":
        devices = [{"name": _sysctl("machdep.cpu.brand_string") or "Apple GPU", "vendor": "apple", "vram_mb": None}]
    if installed("onnxruntime"):
        import onnxruntime

        for ep in onnxruntime.get_available_providers():
            if ep in _ORT_EP:
                runtimes.append("onnxruntime-" + _ORT_EP[ep])
    runtimes.append("cpu-reference")
    vendor = devices[0]["vendor"] if devices else "cpu"
    vrams = [d["vram_mb"] for d in devices if d["vram_mb"] is not None]
    return {
        "vendor": vendor,
        "devices": devices,
        "vram_mb": max(vrams) if vrams else None,
        "runtimes": runtimes,
        "transports": ["shm", "inline"],
        "adapters": available_kinds(),
        "models": list(BUILTIN_MODELS),
    }


def capabilities() -> dict:
    caps = detect()
    if FAKE_VENDOR is None:
        return caps
    # Pose as a GPU box of that vendor (routing tests on loopback).
    fake = {"name": f"Fake {FAKE_VENDOR} GPU", "vendor": FAKE_VENDOR, "vram_mb": 24576}
    runtime = {"nvidia": "torch-cuda", "amd": "torch-rocm"}.get(FAKE_VENDOR)
    return {
        **caps,
        "vendor": FAKE_VENDOR,
        "devices": [fake],
        "vram_mb": fake["vram_mb"],
        "runtimes": [r for r in [runtime] if r] + ["cpu-reference"],
        "adapters": sorted(set(caps["adapters"]) | {"diffusion.sd"}),
    }


def gpu_mem_mb() -> int | None:
    """Memory the GPU runtime holds now, if a GPU runtime is already loaded."""
    torch = sys.modules.get("torch")
    if torch is None:
        return None
    if torch.cuda.is_available() and torch.cuda.is_initialized():
        return torch.cuda.memory_allocated() // (1 << 20)
    if torch.backends.mps.is_available():
        return torch.mps.driver_allocated_memory() // (1 << 20)
    return None
