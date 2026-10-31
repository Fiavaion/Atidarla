#!/bin/sh
# Copies the AI Host an installer ships (PRD A68) into DEST and precompiles
# it with the bundled Python, so an installed app starts its local host with
# no setup. Used by build-dmg.sh and build-appimage.sh; build-msi.ps1 does the
# same in PowerShell.
#
#   packaging/stage-ai-host.sh <dest folder> <bundled python3>
#
# Ships the package source, pyproject.toml, uv.lock and the hash-pinned
# requirements of each optional extra (ai-host/requirements/, exported from
# uv.lock). The base host is standard library only; extras (torch, ONNX
# Runtime, ...) are installed later only on request, into the user's folder
# (`atidarla ai-host install`). No packages and no model weights are shipped.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST=${1:?usage: stage-ai-host.sh <dest> <python>}
PY=${2:?usage: stage-ai-host.sh <dest> <python>}

rm -rf "$DEST"
mkdir -p "$DEST/src"
cp "$ROOT/ai-host/pyproject.toml" "$ROOT/ai-host/uv.lock" "$ROOT/ai-host/README.md" "$DEST/"
cp -R "$ROOT/ai-host/src/atd_ai_host" "$DEST/src/"
cp -R "$ROOT/ai-host/requirements" "$DEST/requirements"
find "$DEST" -name __pycache__ -type d -prune -exec rm -rf {} +
find "$DEST" -name '*.pyc' -delete
# Hash-checked .pyc that Python never rewrites (the host also runs with
# PYTHONDONTWRITEBYTECODE), so nothing is written into a signed bundle.
"$PY" -I -B -m compileall -f -q --invalidation-mode unchecked-hash "$DEST/src" > /dev/null
# No weights and no compiled extensions: source and bytecode only.
if find "$DEST" -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.safetensors' -o -name '*.onnx' -o -name '*.bin' -o -name '*.ckpt' -o -name '*.pt' \) | grep -q .; then
    echo "error: $DEST holds binaries or weights" >&2
    exit 1
fi
