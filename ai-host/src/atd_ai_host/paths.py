"""The per-user model cache (PRD A61)."""

from __future__ import annotations

import os
import sys
from pathlib import Path


def models_dir() -> Path:
    """`ATD_MODELS_DIR`, else the platform cache: never the repo or a project."""
    if os.environ.get("ATD_MODELS_DIR"):
        return Path(os.environ["ATD_MODELS_DIR"]).expanduser()
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "Atidarla" / "models"
    if sys.platform == "win32":
        return Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local")) / "Atidarla" / "models"
    return Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "atidarla" / "models"


def resolve_model(name_or_path: str) -> Path:
    """A model folder: the path itself if it exists, else that name in the model cache."""
    p = Path(name_or_path).expanduser()
    return p if p.exists() else models_dir() / name_or_path
