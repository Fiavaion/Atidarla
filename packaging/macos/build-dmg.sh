#!/bin/sh
# Builds Lasair.app and a DMG (FR-H5). Unsigned (ad hoc) by default; signs,
# notarises and staples when the identity and credentials below are set.
# Never installs anything and never needs sudo. See docs/install.md.
#
# The app carries its own CPython (PRD A68): the pinned python-build-standalone
# runtime (packaging/python-runtime.env) in Contents/Resources/python. The
# release binaries are built against that runtime and find its libpython
# through an rpath; lsr-py starts it with that folder as its home. The AI
# Host's source sits beside it in Contents/Resources/ai-host
# (packaging/stage-ai-host.sh), so the supervisor starts it with no setup.
#
#   packaging/macos/build-dmg.sh            # release build, then dist/*.dmg
#
# Environment (all optional):
#   LSR_VERSION               version in Info.plist and the file name
#                             (default: the workspace version in Cargo.toml)
#   LSR_BUNDLE_ID             CFBundleIdentifier (default org.lasair.Lasair)
#   LSR_SKIP_BUILD=1          use the binaries already in the target folder
#                             (they must have been built against the bundled
#                             runtime: PYO3_CONFIG_FILE=$LSR_DIST/tools/
#                             python-<version>-<release>-<triple>/pyo3-config.txt)
#   CARGO_TARGET_DIR          as for cargo (default: target)
#   LSR_DIST                  output folder (default: dist)
#   LSR_MACOS_SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)"; signs
#                             with the hardened runtime and a timestamp
#   LSR_NOTARY_PROFILE        a notarytool keychain profile
#                             (xcrun notarytool store-credentials), or:
#   LSR_NOTARY_APPLE_ID, LSR_NOTARY_TEAM_ID, LSR_NOTARY_PASSWORD
#                             Apple ID, team and app-specific password (CI)
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
DIST=${LSR_DIST:-$ROOT/dist}
VERSION=${LSR_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}
BUNDLE_ID=${LSR_BUNDLE_ID:-org.lasair.Lasair}
ARCH=$(uname -m)
IDENTITY=${LSR_MACOS_SIGN_IDENTITY:-}
case "$ARCH" in
    arm64) TRIPLE=aarch64-apple-darwin ;;
    *) TRIPLE=$ARCH-apple-darwin ;;
esac

# The bundled runtime, checked against its pinned SHA-256.
PY_HOME=$(packaging/fetch-python.sh "$TRIPLE" "$DIST/tools")
if [ "${LSR_SKIP_BUILD:-}" != "1" ]; then
    PYO3_CONFIG_FILE="$PY_HOME/../pyo3-config.txt" cargo build --release -p lsr-app -p lsr-cli
fi
REL="$TARGET/release"
for bin in lasair-app lasair; do
    [ -x "$REL/$bin" ] || { echo "error: $REL/$bin is missing (build first)" >&2; exit 1; }
done
# The Syphon framework lsr-interop's build script made; loaded at run time
# from Contents/Frameworks (lsr_interop::syphon).
SYPHON=$(ls -dt "$REL"/build/lsr-interop-*/out/syphon-build/Release/Syphon.framework 2>/dev/null | head -n 1 || true)
[ -n "$SYPHON" ] || { echo "error: no Syphon.framework under $REL/build (build lsr-app first)" >&2; exit 1; }

STAGE="$DIST/stage-macos"
APP="$STAGE/Lasair.app"
rm -rf "$STAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"

sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" \
    packaging/macos/Info.plist.in > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" > /dev/null
# The app and the CLI side by side, so `lasair run` finds the app.
cp "$REL/lasair-app" "$REL/lasair" "$APP/Contents/MacOS/"
cp -R "$SYPHON" "$APP/Contents/Frameworks/"
cp LICENSE-MIT LICENSE-APACHE "$APP/Contents/Resources/"
cp third-party/Syphon/License.txt "$APP/Contents/Resources/Syphon-License.txt"

# The Python runtime. Precompile the standard library (hash-checked .pyc that
# are never rewritten), so Python does not write __pycache__ into the signed
# bundle at run time.
PY="$APP/Contents/Resources/python"
cp -R "$PY_HOME" "$PY"
cp "$PY"/lib/python3.*/LICENSE.txt "$APP/Contents/Resources/Python-License.txt"
# -B and -f: the modules compileall itself imports would otherwise get
# timestamp .pyc first, which compileall then keeps, and Python rewrites.
"$PY/bin/python3" -I -B -m compileall -f -q -j 0 --invalidation-mode unchecked-hash "$PY"/lib/python3.* > /dev/null
# Each executable finds libpython (@rpath/libpython3.N.dylib) in the bundle.
for bin in lasair-app lasair; do
    install_name_tool -add_rpath @executable_path/../Resources/python/lib "$APP/Contents/MacOS/$bin"
    if otool -L "$APP/Contents/MacOS/$bin" | grep -E -q '/opt/homebrew|/usr/local|/Library/Frameworks/Python|Python.framework'; then
        echo "error: $bin links a Python outside the bundle (built against another PYO3_PYTHON?):" >&2
        otool -L "$APP/Contents/MacOS/$bin" >&2
        exit 1
    fi
    otool -L "$APP/Contents/MacOS/$bin" | grep -q '@rpath/libpython3' ||
        { echo "error: $bin does not link @rpath/libpython3" >&2; exit 1; }
done
# The AI Host (source, lock, extras' pinned requirements; precompiled), so
# the app starts its local host with no setup. Extras the user installs go
# to ~/Library/Application Support/Lasair/ai-host, never into the bundle.
packaging/stage-ai-host.sh "$APP/Contents/Resources/ai-host" "$PY/bin/python3"
cp -R examples "$APP/Contents/Resources/examples"
find "$APP/Contents/Resources/examples" -name .cache -type d -prune -exec rm -rf {} +

# Icon: the SVG rendered at every size iconutil wants.
ICONSET="$STAGE/Lasair.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -s format png -z $s $s packaging/icons/lasair.svg --out "$ICONSET/icon_${s}x${s}.png" > /dev/null
    d=$((s * 2))
    sips -s format png -z $d $d packaging/icons/lasair.svg --out "$ICONSET/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Lasair.icns"
rm -rf "$ICONSET"

# The runtime's Mach-O files (libpython, extension modules, the python3.N
# program) and any in the AI Host folder (none today: it is source only),
# signed before the bundle that seals them.
MACHO="$STAGE/macho.txt"
find "$PY" "$APP/Contents/Resources/ai-host" -type f \( -name '*.dylib' -o -name '*.so' -o -perm -u+x \) -print | while IFS= read -r f; do
    if file -b "$f" | grep -q Mach-O; then printf '%s\n' "$f"; fi
done > "$MACHO"

# Sign inside out: the runtime, the framework, the executables, then the bundle.
if [ -n "$IDENTITY" ]; then
    echo "signing with $IDENTITY"
    while IFS= read -r f; do
        codesign --force --timestamp --options runtime \
            --entitlements packaging/macos/entitlements.plist --sign "$IDENTITY" "$f"
    done < "$MACHO"
    codesign --force --timestamp --options runtime --sign "$IDENTITY" "$APP/Contents/Frameworks/Syphon.framework"
    for bin in lasair lasair-app; do
        codesign --force --timestamp --options runtime \
            --entitlements packaging/macos/entitlements.plist \
            --sign "$IDENTITY" "$APP/Contents/MacOS/$bin"
    done
    codesign --force --timestamp --options runtime \
        --entitlements packaging/macos/entitlements.plist \
        --sign "$IDENTITY" "$APP"
else
    echo "no LSR_MACOS_SIGN_IDENTITY: ad hoc signature (Gatekeeper will ask; see docs/install.md)"
    while IFS= read -r f; do codesign --force --sign - "$f"; done < "$MACHO"
    codesign --force --sign - "$APP/Contents/Frameworks/Syphon.framework"
    codesign --force --sign - "$APP/Contents/MacOS/lasair"
    codesign --force --sign - "$APP/Contents/MacOS/lasair-app"
    codesign --force --sign - "$APP"
fi
rm "$MACHO"
codesign --verify --deep --strict --verbose=1 "$APP"

# The disk image: the app, a link to /Applications, the examples.
ln -s /Applications "$STAGE/Applications"
cp -R "$APP/Contents/Resources/examples" "$STAGE/Examples"
DMG="$DIST/Lasair-$VERSION-macos-$ARCH.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "Lasair $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

if [ -n "$IDENTITY" ]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
    NOTARY=""
    if [ -n "${LSR_NOTARY_PROFILE:-}" ]; then
        NOTARY="--keychain-profile $LSR_NOTARY_PROFILE"
    elif [ -n "${LSR_NOTARY_APPLE_ID:-}" ]; then
        NOTARY="--apple-id $LSR_NOTARY_APPLE_ID --team-id ${LSR_NOTARY_TEAM_ID:?} --password ${LSR_NOTARY_PASSWORD:?}"
    fi
    if [ -n "$NOTARY" ]; then
        echo "notarising (this waits for Apple)"
        # shellcheck disable=SC2086 # NOTARY holds several arguments
        xcrun notarytool submit "$DMG" $NOTARY --wait
        xcrun stapler staple "$DMG"
        xcrun stapler validate "$DMG"
    else
        echo "signed but not notarised: set LSR_NOTARY_PROFILE or LSR_NOTARY_APPLE_ID/TEAM_ID/PASSWORD"
    fi
fi
rm -rf "$STAGE"
shasum -a 256 "$DMG"
echo "$DMG"
