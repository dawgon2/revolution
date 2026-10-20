#!/usr/bin/env bash
# Revolution app icon (fork rebrand, FORK-PLAN §1a.103): every icon size from
# assets/app-icon/revolution-photo.png (square crop of a public-domain photograph, see its
# .attribution), as a rounded square like a macOS app icon (corner radius 112/512 of the side,
# the same shape upstream's icons use). Overwrites the upstream icon files in place (same paths and
# names, so packaging and the code that embeds them don't change). Needs ImageMagick 7 (`magick`).
# upstream's packaging/icons.sh renders FilmCraft's owl from filmcraft.svg: don't run it in the fork.
#
#   tools/fork/revolution-icon.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIR="$ROOT/assets/app-icon"
SRC="$DIR/revolution-photo.png"
APP_ID="io.github.dawgon2.Revolution" # the packaged icon names (FORK-PLAN §1a.107)
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 1024 px master: the photo full bleed, rounded corners (radius 224 = 112/512 of 1024).
magick "$SRC" -resize 1024x1024 -colorspace sRGB "$TMP/sq.png"
magick -size 1024x1024 xc:black -fill white -draw "roundrectangle 0,0 1023,1023 224,224" "$TMP/mask.png"
magick "$TMP/sq.png" "$TMP/mask.png" -alpha off -compose CopyOpacity -composite -strip "$TMP/tile.png"
# a hairline inner edge so the near-white sky keeps its shape on light backgrounds
magick "$TMP/tile.png" \( -size 1024x1024 xc:none -fill none -stroke 'rgba(0,0,0,0.28)' -strokewidth 6 -draw "roundrectangle 3,3 1020,1020 221,221" \) -compose Over -composite -strip "$TMP/tile.png"
cp "$TMP/tile.png" "$DIR/filmcraft-1024.png"

render() { magick "$TMP/tile.png" -filter Lanczos -resize "$1x$1" -strip "PNG32:$2"; }

# macOS style (owner, 2026-10-20, FORK-PLAN §1a.108: the default desktop icon everywhere): Apple's
# grid, tile body 824 of 1024 with a transparent margin and a soft drop shadow under the tile.
magick "$TMP/tile.png" -resize 824x824 -background none -gravity center -extent 1024x1024 "$TMP/body.png"
magick "$TMP/body.png" \( +clone -background black -shadow 50x14+0+12 \) +swap -background none -layers merge +repage \
  -gravity center -crop 1024x1024+0+0 +repage -extent 1024x1024 -strip "$TMP/mac.png"
magick "$TMP/mac.png" -filter Lanczos -resize 512x512 -strip "PNG32:$DIR/filmcraft-macos-512.png"
mac() { magick "$TMP/mac.png" -filter Lanczos -resize "$1x$1" -strip "PNG32:$2"; }

# hicolor: the macOS-style tile; 16 and 24 px keep the full-bleed tile (a margin would leave a
# speck). The square variant (Settings ▸ Appearance ▸ App icon ▸ Square) is made by the app.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  if [ "$s" -le 24 ]; then render "$s" "$DIR/hicolor/${s}x${s}/apps/$APP_ID.png"; else mac "$s" "$DIR/hicolor/${s}x${s}/apps/$APP_ID.png"; fi
done
# "scalable": the 512 px tile inside an SVG (desktops prefer the scalable icon)
mac 512 "$TMP/512.png"
mkdir -p "$DIR/hicolor/scalable/apps"
{
  printf '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 512 512" width="512" height="512">'
  printf '<image width="512" height="512" xlink:href="data:image/png;base64,'
  base64 -w0 "$TMP/512.png"
  printf '"/></svg>\n'
} >"$DIR/hicolor/scalable/apps/$APP_ID.svg"

render 128 "$ROOT/apps/filmcraft-web/web/favicon.png"

ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  if [ "$s" -le 24 ]; then render "$s" "$TMP/ico-$s.png"; else mac "$s" "$TMP/ico-$s.png"; fi
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/filmcraft.ico" "${ICO_PNGS[@]}")
echo "revolution-icon: done (macOS .icns not regenerated: needs iconutil)"
