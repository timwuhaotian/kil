#!/bin/sh
# Assembles Kil.app around the cargo binary, with the icon rendered from packaging/icon.svg.
# Usage: scripts/bundle.sh [debug|release]   → prints the .app path
set -eu

profile="${1:-debug}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

command -v rsvg-convert >/dev/null || { echo "needs rsvg-convert: brew install librsvg" >&2; exit 1; }
case "$profile" in
  debug) cargo build -p kil ;;
  release) cargo build --release -p kil ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

out="target/$profile"
app="$out/Kil.app"
version="$(cargo pkgid -p kil | sed 's/.*[#@]//')"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$out/kil" "$app/Contents/MacOS/kil"
sed "s/__VERSION__/$version/g" packaging/Info.plist > "$app/Contents/Info.plist"

iconset="$out/AppIcon.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size" -h "$size" packaging/icon.svg -o "$iconset/icon_${size}x${size}.png"
  double=$((size * 2))
  rsvg-convert -w "$double" -h "$double" packaging/icon.svg -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

# Ad-hoc seal for local runs; scripts/release.sh re-signs with Developer ID (DESIGN.md §11).
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
