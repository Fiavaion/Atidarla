"""The installers ship one hash-pinned requirements file per extra.

`requirements/<extra>.txt` is what an installed app's `atidarla ai-host
install <extra>` hands to pip (`--require-hashes --no-deps`), so each must
be exactly `uv export` of `uv.lock` for that extra. Regenerate with:

    uv export --frozen --extra <extra> --no-dev --no-emit-project --no-header \
        --format requirements-txt -q -o requirements/<extra>.txt
"""

from __future__ import annotations

import re
import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


def extras() -> list[str]:
    project = tomllib.loads((ROOT / "pyproject.toml").read_text())["project"]
    return sorted(project.get("optional-dependencies", {}))


def test_every_extra_has_a_requirements_file_and_no_other():
    shipped = sorted(p.stem for p in (ROOT / "requirements").glob("*.txt"))
    assert shipped == extras()


@pytest.mark.parametrize("extra", extras())
def test_requirements_pin_every_package_by_hash(extra):
    text = (ROOT / "requirements" / f"{extra}.txt").read_text()
    entries = [line for line in text.splitlines() if line and not line[0].isspace() and not line.startswith("#")]
    assert entries, extra
    for line in entries:
        assert "==" in line, line
    # Each pinned entry is followed by at least one hash.
    assert text.count("--hash=sha256:") >= len(entries)
    assert not any(line.startswith("atd-ai-host") for line in entries), "the host ships as source"


def torch_pins(extra: str) -> dict[str, str]:
    """`torch` version per marker in an extra's requirements."""
    text = (ROOT / "requirements" / f"{extra}.txt").read_text()
    return {
        marker.strip(): version
        for version, marker in re.findall(r"^torch==(\S+)(?: ;([^\\\n]*))?", text, flags=re.MULTILINE)
    }


def test_the_cuda_extra_takes_torch_from_pytorchs_cuda_index_on_windows_and_linux_only():
    # `atd_ai::hostenv::PYTORCH_INDEX` passes this index to pip.
    pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text())
    (index,) = pyproject["tool"]["uv"]["index"]
    assert index["explicit"] is True
    (source,) = pyproject["tool"]["uv"]["sources"]["torch"]
    assert source == {
        "index": index["name"],
        "extra": "diffusion-cuda",
        "marker": "sys_platform == 'win32' or sys_platform == 'linux'",
    }
    assert [{"extra": "diffusion"}, {"extra": "diffusion-cuda"}] in pyproject["tool"]["uv"]["conflicts"]
    cuda_tag = index["url"].rsplit("/", 1)[1]
    pins = torch_pins("diffusion-cuda")
    assert pins["sys_platform == 'linux' or sys_platform == 'win32'"].endswith(f"+{cuda_tag}")
    assert "+" not in pins["sys_platform != 'linux' and sys_platform != 'win32'"], "macOS keeps PyPI's torch"
    (standard,) = torch_pins("diffusion").values()
    assert "+" not in standard
    # Same torch release in both, so the adapter sees one API.
    assert {v.split("+")[0] for v in pins.values()} == {standard}


@pytest.mark.skipif(shutil.which("uv") is None, reason="needs uv")
@pytest.mark.parametrize("extra", extras())
def test_requirements_match_the_lock(extra):
    exported = subprocess.run(
        [
            "uv",
            "export",
            "--frozen",
            "--extra",
            extra,
            "--no-dev",
            "--no-emit-project",
            "--no-header",
            "--format",
            "requirements-txt",
            "-q",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    assert exported == (ROOT / "requirements" / f"{extra}.txt").read_text(), (
        f"requirements/{extra}.txt is stale: regenerate it (see this file's docstring)"
    )
