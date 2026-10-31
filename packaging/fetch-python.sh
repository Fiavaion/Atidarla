#!/bin/sh
# Downloads, checks and unpacks the pinned CPython runtime
# (packaging/python-runtime.env, PRD A68) for one target, and prints the
# runtime home (the folder holding bin/ and lib/). Used by build-dmg.sh and
# build-appimage.sh; the Windows script does the same in PowerShell.
#
#   packaging/fetch-python.sh aarch64-apple-darwin dist/tools
#
# The archive is kept in the cache folder and checked against its pinned
# SHA-256 on every run; a mismatch deletes it and fails the build.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=python-runtime.env
. "$HERE/python-runtime.env"
TRIPLE=${1:?usage: fetch-python.sh <target-triple> <cache-folder>}
CACHE=${2:?usage: fetch-python.sh <target-triple> <cache-folder>}

KEY=PBS_SHA256_$(printf '%s' "$TRIPLE" | tr -- '-' '_')
eval "SHA=\${$KEY:-}"
[ -n "$SHA" ] || { echo "error: python-runtime.env pins no Python for $TRIPLE" >&2; exit 1; }

NAME="cpython-$PBS_PYTHON+$PBS_RELEASE-$TRIPLE-install_only_stripped.tar.gz"
URL="https://github.com/astral-sh/python-build-standalone/releases/download/$PBS_RELEASE/cpython-$PBS_PYTHON%2B$PBS_RELEASE-$TRIPLE-install_only_stripped.tar.gz"
mkdir -p "$CACHE"
ARCHIVE="$CACHE/$NAME"

sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

if [ ! -f "$ARCHIVE" ]; then
    echo "downloading $NAME" >&2
    curl -fsSL --retry 3 -o "$ARCHIVE.part" "$URL"
    mv "$ARCHIVE.part" "$ARCHIVE"
fi
GOT=$(sha256 "$ARCHIVE")
if [ "$GOT" != "$SHA" ]; then
    rm -f "$ARCHIVE"
    echo "error: $NAME has SHA-256 $GOT, expected $SHA (python-runtime.env)" >&2
    exit 1
fi

OUT="$CACHE/python-$PBS_PYTHON-$PBS_RELEASE-$TRIPLE"
if [ "$(cat "$OUT/.sha256" 2> /dev/null || true)" != "$SHA" ]; then
    rm -rf "$OUT"
    mkdir -p "$OUT"
    tar -xzf "$ARCHIVE" -C "$OUT"
    # No Tk GUI toolkit (tkinter, IDLE, turtle and the Tcl/Tk libraries):
    # nothing in Atidarla uses it (Mark, 2026-10-31). pip and the C headers
    # stay so native packages can still be built against this Python.
    (
        cd "$OUT/python"
        rm -rf lib/tcl* lib/tk* lib/itcl* lib/thread* lib/libtcl* lib/libtk* \
            lib/python3.*/tkinter lib/python3.*/idlelib lib/python3.*/turtledemo \
            lib/python3.*/turtle.py lib/python3.*/lib-dynload/_tkinter* bin/idle3*
    )
    printf '%s\n' "$SHA" > "$OUT/.sha256"
fi
[ -d "$OUT/python/lib" ] || { echo "error: $ARCHIVE did not unpack to python/" >&2; exit 1; }

# A PyO3 build configuration for linking against this runtime. Its sysconfig
# names the folder it was built in (/install/lib), so PYO3_PYTHON alone
# would send the linker there; this file gives the real lib_dir.
MINOR=$(printf '%s' "$PBS_PYTHON" | cut -d . -f 1-2)
cat > "$OUT/pyo3-config.txt" << EOF
implementation=CPython
version=$MINOR
shared=true
abi3=false
lib_name=python$MINOR
lib_dir=$(cd "$OUT/python/lib" && pwd)
executable=$(cd "$OUT/python/bin" && pwd)/python3
pointer_width=64
build_flags=
suppress_build_script_link_lines=false
EOF
echo "$OUT/python"
