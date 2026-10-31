"""The installers ship one hash-pinned requirements file per extra.

`requirements/<extra>.txt` is what an installed app's `atidarla ai-host
install <extra>` hands to pip (`--require-hashes --no-deps`), so each must
be exactly `uv export` of `uv.lock` for that extra. Regenerate with:

    uv export --frozen --extra <extra> --no-dev --no-emit-project --no-header \
        --format requirements-txt -q -o requirements/<extra>.txt
"""

from __future__ import annotations

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
