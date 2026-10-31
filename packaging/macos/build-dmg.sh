#!/bin/sh
# Builds Atidarla.app and a DMG (FR-H5). Unsigned (ad hoc) by default; signs,
# notarises and staples when the identity and credentials below are set.
# Never installs anything and never needs sudo. See docs/install.md.
#
#   packaging/macos/build-dmg.sh            # release build, then dist/*.dmg
#
# Environment (all optional):
#   ATD_VERSION               version in Info.plist and the file name
#                             (default: the workspace version in Cargo.toml)
#   ATD_BUNDLE_ID             CFBundleIdentifier (default org.atidarla.Atidarla)
#   ATD_SKIP_BUILD=1          use the binaries already in the target folder
#   CARGO_TARGET_DIR          as for cargo (default: target)
#   ATD_DIST                  output folder (default: dist)
#   ATD_MACOS_SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)"; signs
#                             with the hardened runtime and a timestamp
#   ATD_NOTARY_PROFILE        a notarytool keychain profile
#                             (xcrun notarytool store-credentials), or:
#   ATD_NOTARY_APPLE_ID, ATD_NOTARY_TEAM_ID, ATD_NOTARY_PASSWORD
#                             Apple ID, team and app-specific password (CI)
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
DIST=${ATD_DIST:-$ROOT/dist}
VERSION=${ATD_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}
BUNDLE_ID=${ATD_BUNDLE_ID:-org.atidarla.Atidarla}
ARCH=$(uname -m)
IDENTITY=${ATD_MACOS_SIGN_IDENTITY:-}

if [ "${ATD_SKIP_BUILD:-}" != "1" ]; then
    cargo build --release -p atd-app -p atd-cli
fi
REL="$TARGET/release"
for bin in atidarla-app atidarla; do
    [ -x "$REL/$bin" ] || { echo "error: $REL/$bin is missing (build first)" >&2; exit 1; }
done
# The Syphon framework atd-interop's build script made; loaded at run time
# from Contents/Frameworks (atd_interop::syphon).
SYPHON=$(ls -dt "$REL"/build/atd-interop-*/out/syphon-build/Release/Syphon.framework 2>/dev/null | head -n 1 || true)
[ -n "$SYPHON" ] || { echo "error: no Syphon.framework under $REL/build (build atd-app first)" >&2; exit 1; }

STAGE="$DIST/stage-macos"
APP="$STAGE/Atidarla.app"
rm -rf "$STAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"

sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" \
    packaging/macos/Info.plist.in > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" > /dev/null
# The app and the CLI side by side, so `atidarla run` finds the app.
cp "$REL/atidarla-app" "$REL/atidarla" "$APP/Contents/MacOS/"
cp -R "$SYPHON" "$APP/Contents/Frameworks/"
cp LICENSE-MIT LICENSE-APACHE "$APP/Contents/Resources/"
cp third-party/Syphon/License.txt "$APP/Contents/Resources/Syphon-License.txt"
cp -R examples "$APP/Contents/Resources/examples"
find "$APP/Contents/Resources/examples" -name .cache -type d -prune -exec rm -rf {} +

# Icon: the SVG rendered at every size iconutil wants.
ICONSET="$STAGE/Atidarla.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -s format png -z $s $s packaging/icons/atidarla.svg --out "$ICONSET/icon_${s}x${s}.png" > /dev/null
    d=$((s * 2))
    sips -s format png -z $d $d packaging/icons/atidarla.svg --out "$ICONSET/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Atidarla.icns"
rm -rf "$ICONSET"

# Sign inside out: the framework, the executables, then the bundle.
if [ -n "$IDENTITY" ]; then
    echo "signing with $IDENTITY"
    codesign --force --timestamp --options runtime --sign "$IDENTITY" "$APP/Contents/Frameworks/Syphon.framework"
    for bin in atidarla atidarla-app; do
        codesign --force --timestamp --options runtime \
            --entitlements packaging/macos/entitlements.plist \
            --sign "$IDENTITY" "$APP/Contents/MacOS/$bin"
    done
    codesign --force --timestamp --options runtime \
        --entitlements packaging/macos/entitlements.plist \
        --sign "$IDENTITY" "$APP"
else
    echo "no ATD_MACOS_SIGN_IDENTITY: ad hoc signature (Gatekeeper will ask; see docs/install.md)"
    codesign --force --sign - "$APP/Contents/Frameworks/Syphon.framework"
    codesign --force --sign - "$APP/Contents/MacOS/atidarla"
    codesign --force --sign - "$APP/Contents/MacOS/atidarla-app"
    codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=1 "$APP"

# The disk image: the app, a link to /Applications, the examples.
ln -s /Applications "$STAGE/Applications"
cp -R "$APP/Contents/Resources/examples" "$STAGE/Examples"
DMG="$DIST/Atidarla-$VERSION-macos-$ARCH.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "Atidarla $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

if [ -n "$IDENTITY" ]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
    NOTARY=""
    if [ -n "${ATD_NOTARY_PROFILE:-}" ]; then
        NOTARY="--keychain-profile $ATD_NOTARY_PROFILE"
    elif [ -n "${ATD_NOTARY_APPLE_ID:-}" ]; then
        NOTARY="--apple-id $ATD_NOTARY_APPLE_ID --team-id ${ATD_NOTARY_TEAM_ID:?} --password ${ATD_NOTARY_PASSWORD:?}"
    fi
    if [ -n "$NOTARY" ]; then
        echo "notarising (this waits for Apple)"
        # shellcheck disable=SC2086 # NOTARY holds several arguments
        xcrun notarytool submit "$DMG" $NOTARY --wait
        xcrun stapler staple "$DMG"
        xcrun stapler validate "$DMG"
    else
        echo "signed but not notarised: set ATD_NOTARY_PROFILE or ATD_NOTARY_APPLE_ID/TEAM_ID/PASSWORD"
    fi
fi
rm -rf "$STAGE"
shasum -a 256 "$DMG"
echo "$DMG"
