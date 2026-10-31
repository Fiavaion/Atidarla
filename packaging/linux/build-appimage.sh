#!/bin/sh
# Builds an AppImage of Atidarla (FR-H5, Linux best-effort). Bundles the app,
# the CLI, the examples and CPython (the pinned python-build-standalone
# runtime, packaging/python-runtime.env, in usr/lib/atidarla/python; PRD
# A68) and the AI Host in usr/lib/atidarla/ai-host; system libraries (Vulkan driver, ALSA, X11 or Wayland) come from the
# host. Never installs anything.
#
#   packaging/linux/build-appimage.sh
#
# Environment (all optional):
#   ATD_VERSION        version in the file name (default: the workspace version)
#   ATD_SKIP_BUILD=1   use the binaries already in the target folder
#   CARGO_TARGET_DIR   as for cargo (default: target)
#   ATD_DIST           output folder (default: dist)
#   APPIMAGETOOL       path to appimagetool; otherwise it is downloaded into
#                      $ATD_DIST/tools from the AppImage project's releases
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
DIST=${ATD_DIST:-$ROOT/dist}
VERSION=${ATD_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}
ARCH=$(uname -m)

PY_HOME=$(packaging/fetch-python.sh "$ARCH-unknown-linux-gnu" "$DIST/tools")
if [ "${ATD_SKIP_BUILD:-}" != "1" ]; then
    # Linked against the bundled runtime, found through the rpath below.
    PYO3_CONFIG_FILE="$PY_HOME/../pyo3-config.txt" \
        RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,\$ORIGIN/../lib/atidarla/python/lib" \
        cargo build --release -p atd-app -p atd-cli
fi
REL="$TARGET/release"

APPDIR="$DIST/stage-linux/Atidarla.AppDir"
rm -rf "$DIST/stage-linux"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/atidarla" "$APPDIR/usr/lib/atidarla"
cp "$REL/atidarla-app" "$REL/atidarla" "$APPDIR/usr/bin/"
cp -R examples "$APPDIR/usr/share/atidarla/examples"
cp LICENSE-MIT LICENSE-APACHE "$APPDIR/usr/share/atidarla/"
cp -R "$PY_HOME" "$APPDIR/usr/lib/atidarla/python"
cp "$PY_HOME"/lib/python3.*/LICENSE.txt "$APPDIR/usr/share/atidarla/Python-License.txt"
# The AppImage is read-only: precompile so imports do not recompile each run.
"$APPDIR/usr/lib/atidarla/python/bin/python3" -I -B -m compileall -f -q -j 0 \
    --invalidation-mode unchecked-hash "$APPDIR"/usr/lib/atidarla/python/lib/python3.* > /dev/null
# The AI Host (source, lock, extras' pinned requirements), started by the
# supervisor with no setup; extras the user installs go to
# $XDG_DATA_HOME/atidarla/ai-host.
packaging/stage-ai-host.sh "$APPDIR/usr/lib/atidarla/ai-host" "$APPDIR/usr/lib/atidarla/python/bin/python3"
cp packaging/icons/atidarla.svg "$APPDIR/atidarla.svg"
# appimagetool wants a raster .DirIcon; render one when rsvg-convert is there.
if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w 256 -h 256 packaging/icons/atidarla.svg -o "$APPDIR/atidarla.png"
    rm "$APPDIR/atidarla.svg"
    ln -sf atidarla.png "$APPDIR/.DirIcon"
else
    ln -sf atidarla.svg "$APPDIR/.DirIcon"
fi
cat > "$APPDIR/atidarla.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Atidarla
Comment=Real-time visual instrument
Exec=atidarla-app %F
Icon=atidarla
Categories=Graphics;AudioVideo;
Terminal=false
EOF
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
# A CLI command as the first argument runs the CLI (Atidarla.AppImage soak ...);
# anything else opens the app.
HERE=$(dirname "$(readlink -f "$0")")
case "${1:-}" in
    render|run|serve|soak|bench|bake|check|fmt|docs|registry|models|ai-host|batch|agent|cloud|comfy|kernel|diff|merge|merge-file|isf-import|export-show|verify-show)
        exec "$HERE/usr/bin/atidarla" "$@" ;;
esac
exec "$HERE/usr/bin/atidarla-app" "$@"
EOF
chmod +x "$APPDIR/AppRun"

TOOL=${APPIMAGETOOL:-}
if [ -z "$TOOL" ]; then
    TOOL="$DIST/tools/appimagetool-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
        mkdir -p "$DIST/tools"
        curl -fsSL -o "$TOOL" \
            "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
        chmod +x "$TOOL"
    fi
fi
OUT="$DIST/Atidarla-$VERSION-linux-$ARCH.AppImage"
# --appimage-extract-and-run: works without FUSE (CI containers).
ARCH=$ARCH "$TOOL" --appimage-extract-and-run "$APPDIR" "$OUT"
rm -rf "$DIST/stage-linux"
sha256sum "$OUT"
echo "$OUT"
