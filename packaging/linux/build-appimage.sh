#!/bin/sh
# Builds an AppImage of Atidarla (FR-H5, Linux best-effort). Bundles the app,
# the CLI and the examples; system libraries (Vulkan driver, ALSA, X11 or
# Wayland, Python 3.13) come from the host. Never installs anything.
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

if [ "${ATD_SKIP_BUILD:-}" != "1" ]; then
    cargo build --release -p atd-app -p atd-cli
fi
REL="$TARGET/release"

APPDIR="$DIST/stage-linux/Atidarla.AppDir"
rm -rf "$DIST/stage-linux"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/atidarla"
cp "$REL/atidarla-app" "$REL/atidarla" "$APPDIR/usr/bin/"
cp -R examples "$APPDIR/usr/share/atidarla/examples"
cp LICENSE-MIT LICENSE-APACHE "$APPDIR/usr/share/atidarla/"
cp packaging/icons/atidarla.svg "$APPDIR/atidarla.svg"
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
    render|run|serve|soak|bench|bake|check|fmt|docs|registry|models|batch|agent|cloud|comfy|kernel|diff|merge|merge-file|isf-import|export-show|verify-show)
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
