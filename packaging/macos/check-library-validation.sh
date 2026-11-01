#!/bin/sh
# Checks that the bundled Python of a built DMG or app can load a Python
# extension module that is not signed by our team, from outside the bundle,
# under the hardened runtime: what pip-installed AI Host extras are
# (PRD A70; they are ad hoc or linker-signed with no Team ID and live in the
# user's data folder). Library validation refuses such a module unless the
# Python program carries com.apple.security.cs.disable-library-validation
# (packaging/macos/entitlements.plist). See docs/install.md.
#
#   packaging/macos/check-library-validation.sh dist/Lasair-<v>-macos-<arch>.dmg
#   packaging/macos/check-library-validation.sh path/to/Lasair.app
#
# Works on a copy in a temporary folder; the input is never changed.
# - A Developer ID build is tested as signed.
# - An ad hoc build (the release dry run, no certificate) first has its
#   Python program re-signed ad hoc with the hardened runtime and the repo's
#   entitlements, as build-dmg.sh signs it with a real identity. Ad hoc plus
#   the hardened runtime enforces library validation the same way (checked
#   2026-11-01: "mapping process and mapped file (non-platform) have
#   different Team IDs" without the entitlement).
# Then a control: the same program re-signed without the entitlement must
# refuse the module, or this check could not see a regression.
# Needs only the Xcode command-line tools (cc, codesign, hdiutil).
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
INPUT=${1:?usage: check-library-validation.sh <Lasair.dmg | Lasair.app>}
ENTITLEMENTS="$ROOT/packaging/macos/entitlements.plist"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/lsr-lv.XXXXXX")
MNT=""
cleanup() {
    if [ -n "$MNT" ]; then hdiutil detach -quiet "$MNT" || true; fi
    rm -rf "$WORK"
}
trap cleanup EXIT

case "$INPUT" in
    *.dmg)
        MNT="$WORK/mnt"
        mkdir -p "$MNT"
        hdiutil attach -quiet -nobrowse -readonly -mountpoint "$MNT" "$INPUT"
        cp -R "$MNT/Lasair.app" "$WORK/"
        hdiutil detach -quiet "$MNT"
        MNT=""
        ;;
    *.app) cp -R "$INPUT" "$WORK/Lasair.app" ;;
    *)
        echo "error: $INPUT is neither a .dmg nor a .app" >&2
        exit 2
        ;;
esac

PYHOME="$WORK/Lasair.app/Contents/Resources/python"
PROGRAM=$(cd "$PYHOME/bin" && pwd -P)/$(readlink "$PYHOME/bin/python3" || echo python3)
LIBPYTHON=$(ls "$PYHOME"/lib/libpython3.*.dylib)
INCLUDE=$(ls -d "$PYHOME"/include/python3.*)
SUFFIX=$("$PYHOME/bin/python3" -I -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')

# The probe: a minimal extension module, built here, so it is linker-signed
# (arm64) or unsigned (x86_64) with no Team ID, like a pip wheel's.
mkdir -p "$WORK/ext"
cat > "$WORK/ext/lvprobe.c" << 'EOF'
#include <Python.h>
static struct PyModuleDef probe = {PyModuleDef_HEAD_INIT, "lvprobe", NULL, -1, NULL};
PyMODINIT_FUNC PyInit_lvprobe(void) { return PyModule_Create(&probe); }
EOF
cc -bundle -undefined dynamic_lookup -I"$INCLUDE" -o "$WORK/ext/lvprobe$SUFFIX" "$WORK/ext/lvprobe.c"

# Import the probe the way the supervisor starts the AI Host (hostenv.rs).
probe() {
    env -i HOME="$WORK" PATH=/usr/bin:/bin PYTHONHOME="$PYHOME" PYTHONPATH="$WORK/ext" \
        PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1 \
        "$PYHOME/bin/python3" -c 'import lvprobe' > "$WORK/probe.log" 2>&1
}

FLAGS=$(codesign -dv "$PROGRAM" 2>&1 | sed -n 's/.*flags=[^(]*(\(.*\)).*/\1/p')
case "$FLAGS" in
    *adhoc*)
        echo "ad hoc build: re-signing $(basename "$PROGRAM") ad hoc with the hardened runtime and $ENTITLEMENTS"
        codesign --force --sign - --options runtime --entitlements "$ENTITLEMENTS" "$PROGRAM"
        ;;
    *runtime*) echo "signed build: testing $(basename "$PROGRAM") as signed" ;;
    *)
        echo "error: $PROGRAM is signed without the hardened runtime (flags: $FLAGS)" >&2
        exit 1
        ;;
esac
codesign -d --entitlements - "$PROGRAM" 2> /dev/null | grep -q disable-library-validation ||
    { echo "error: $PROGRAM lacks com.apple.security.cs.disable-library-validation" >&2; exit 1; }

if probe; then
    echo "ok: the bundled Python loads an extension module with no Team ID from outside the bundle"
else
    echo "error: the bundled Python refused an extension module from outside the bundle:" >&2
    cat "$WORK/probe.log" >&2
    exit 1
fi

# Control: without the entitlement the same import must be refused. Both the
# program and libpython are signed ad hoc, so the program still starts.
NO_DLV="$WORK/no-dlv.plist"
cp "$ENTITLEMENTS" "$NO_DLV"
/usr/libexec/PlistBuddy -c "Delete :com.apple.security.cs.disable-library-validation" "$NO_DLV"
codesign --force --sign - "$LIBPYTHON"
codesign --force --sign - --options runtime --entitlements "$NO_DLV" "$PROGRAM"
if probe; then
    echo "error: control failed: the module loaded without disable-library-validation," >&2
    echo "so this check cannot detect library validation on this macOS" >&2
    exit 1
fi
if grep -q "not valid for use in process" "$WORK/probe.log"; then
    echo "ok: control refused by library validation without the entitlement"
else
    echo "error: control failed for another reason:" >&2
    cat "$WORK/probe.log" >&2
    exit 1
fi
