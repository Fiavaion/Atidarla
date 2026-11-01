#!/bin/sh
# Builds an AppImage of Lasair (FR-H5, Linux best-effort). Bundles the app,
# the CLI, the examples and CPython (the pinned python-build-standalone
# runtime, packaging/python-runtime.env, in usr/lib/lasair/python; PRD
# A68) and the AI Host in usr/lib/lasair/ai-host; system libraries (Vulkan driver, ALSA, X11 or Wayland) come from the
# host. Never installs anything.
#
#   packaging/linux/build-appimage.sh
#
# Environment (all optional):
#   LSR_VERSION        version in the file name (default: the workspace version)
#   LSR_SKIP_BUILD=1   use the binaries already in the target folder
#   CARGO_TARGET_DIR   as for cargo (default: target)
#   LSR_DIST           output folder (default: dist)
#   APPIMAGETOOL       path to appimagetool; otherwise it is downloaded into
#                      $LSR_DIST/tools from the AppImage project's releases
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
DIST=${LSR_DIST:-$ROOT/dist}
VERSION=${LSR_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}
ARCH=$(uname -m)

PY_HOME=$(packaging/fetch-python.sh "$ARCH-unknown-linux-gnu" "$DIST/tools")
if [ "${LSR_SKIP_BUILD:-}" != "1" ]; then
    # Linked against the bundled runtime, found through the rpath below.
    PYO3_CONFIG_FILE="$PY_HOME/../pyo3-config.txt" \
        RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,\$ORIGIN/../lib/lasair/python/lib" \
        cargo build --release -p lsr-app -p lsr-cli
fi
REL="$TARGET/release"

APPDIR="$DIST/stage-linux/Lasair.AppDir"
rm -rf "$DIST/stage-linux"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/lasair" "$APPDIR/usr/lib/lasair"
cp "$REL/lasair-app" "$REL/lasair" "$APPDIR/usr/bin/"
cp -R examples "$APPDIR/usr/share/lasair/examples"
cp LICENSE-MIT LICENSE-APACHE "$APPDIR/usr/share/lasair/"
cp -R "$PY_HOME" "$APPDIR/usr/lib/lasair/python"
cp "$PY_HOME"/lib/python3.*/LICENSE.txt "$APPDIR/usr/share/lasair/Python-License.txt"
# The AppImage is read-only: precompile so imports do not recompile each run.
"$APPDIR/usr/lib/lasair/python/bin/python3" -I -B -m compileall -f -q -j 0 \
    --invalidation-mode unchecked-hash "$APPDIR"/usr/lib/lasair/python/lib/python3.* > /dev/null
# The AI Host (source, lock, extras' pinned requirements), started by the
# supervisor with no setup; extras the user installs go to
# $XDG_DATA_HOME/lasair/ai-host.
packaging/stage-ai-host.sh "$APPDIR/usr/lib/lasair/ai-host" "$APPDIR/usr/lib/lasair/python/bin/python3"
cp packaging/icons/lasair.svg "$APPDIR/lasair.svg"
# appimagetool wants a raster .DirIcon; render one when rsvg-convert is there.
if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w 256 -h 256 packaging/icons/lasair.svg -o "$APPDIR/lasair.png"
    rm "$APPDIR/lasair.svg"
    ln -sf lasair.png "$APPDIR/.DirIcon"
else
    ln -sf lasair.svg "$APPDIR/.DirIcon"
fi
cat > "$APPDIR/lasair.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Lasair
Comment=Real-time visual instrument
Exec=lasair-app %F
Icon=lasair
Categories=Graphics;AudioVideo;
Terminal=false
EOF
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
# A CLI command as the first argument runs the CLI (Lasair.AppImage soak ...);
# anything else opens the app.
HERE=$(dirname "$(readlink -f "$0")")
case "${1:-}" in
    render|run|serve|soak|bench|bake|check|fmt|docs|registry|models|ai-host|batch|agent|cloud|comfy|kernel|diff|merge|merge-file|isf-import|export-show|verify-show)
        exec "$HERE/usr/bin/lasair" "$@" ;;
esac
exec "$HERE/usr/bin/lasair-app" "$@"
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
OUT="$DIST/Lasair-$VERSION-linux-$ARCH.AppImage"
# --appimage-extract-and-run: works without FUSE (CI containers).
ARCH=$ARCH "$TOOL" --appimage-extract-and-run "$APPDIR" "$OUT"
rm -rf "$DIST/stage-linux"
sha256sum "$OUT"
echo "$OUT"
