#!/bin/bash
# Renders the mockups with headless Chrome: full screens at 1920×1080 (de and en), component sheets
# at 2x. Usage: ./render.sh [name …] (default: all .html files here).
cd "$(dirname "$0")"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
shot() { # file lang w h scale out
  local hash=""; [ "$2" = en ] && hash="#en"
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files \
    --window-size="$3,$4" --force-device-scale-factor="$5" --virtual-time-budget=2000 \
    --screenshot="$PWD/$6" "file://$PWD/$1$hash" >/dev/null 2>&1
}
files="$*"; [ -z "$files" ] && files=$(ls [0-9]*.html)
for f in $files; do
  base="${f%.html}"
  size=$(grep -o 'data-size="[0-9]*x[0-9]*x[0-9]*"' "$f" | head -1 | tr -dc '0-9x')
  [ -z "$size" ] && size="1920x1080x1"
  IFS=x read -r w h s <<< "$size"
  shot "$f" de "$w" "$h" "$s" "$base.png"
  shot "$f" en "$w" "$h" "$s" "$base-en.png"
  echo "$base.png $base-en.png"
done
