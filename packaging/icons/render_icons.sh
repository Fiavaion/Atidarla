#!/bin/sh
# Renders the committed raster icons from lasair.svg (PRD A69: concept P3,
# approved by Mark 2026-11-01):
#   png/lasair-<size>.png  16, 24, 32, 48, 64, 128, 256 px
#   lasair.ico             those seven sizes (make_ico.py)
# The renderer is macOS sips (sips-316 on macOS 26 made the committed files;
# its 16, 32, 64 and 128 px renders are byte for byte the previews Mark
# approved). They are committed so Windows builds need no SVG rasteriser.
# build-dmg.sh renders the .icns from the SVG with sips itself, and the
# AppImage with rsvg-convert. Run this after changing make_icon.py:
#
#   python3 packaging/icons/make_icon.py && packaging/icons/render_icons.sh
set -eu

DIR=$(cd "$(dirname "$0")" && pwd)
command -v sips >/dev/null || { echo "error: needs macOS sips" >&2; exit 1; }
mkdir -p "$DIR/png"
for s in 16 24 32 48 64 128 256; do
    sips -s format png -z "$s" "$s" "$DIR/lasair.svg" --out "$DIR/png/lasair-$s.png" > /dev/null
done
python3 "$DIR/make_ico.py" "$DIR"
