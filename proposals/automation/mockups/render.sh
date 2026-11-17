#!/bin/bash
# usage: render.sh <html-file> <out.png> [hash] [w h]
D="$(cd "$(dirname "$0")" && pwd)"
W=${4:-1920}; H=${5:-1080}
OUT="$D/$2"
rm -f "$OUT"
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars \
  --force-device-scale-factor=2 --allow-file-access-from-files \
  --user-data-dir="${TMPDIR:-/tmp}/komponist-mockups-chrome" \
  --window-size=$W,$H --screenshot="$OUT" "file://$D/$1$3" >/dev/null 2>&1 &
PID=$!
for i in $(seq 1 120); do
  if [ -s "$OUT" ]; then perl -e 'select(undef,undef,undef,0.5)'; break; fi
  kill -0 $PID 2>/dev/null || break
  perl -e 'select(undef,undef,undef,0.25)'
done
kill $PID 2>/dev/null; wait $PID 2>/dev/null
ls -la "$OUT"
# All mockups (de + en):
#   ./render.sh 01-arrangement-lanes.html 01-arrangement-lanes.png; ./render.sh 01-arrangement-lanes.html 01-arrangement-lanes-en.png "#en"
#   ./render.sh 01-arrangement-lanes.html 02-parameter-chooser.png "#chooser"; ... 02-parameter-chooser-en.png "#chooser-en"
#   ./render.sh 03-header-states.html 03-header-states.png "" 1400 860; ... 03-header-states-en.png "#en" 1400 860
#   ./render.sh 04-clip-automation.html 04-clip-automation.png; ./render.sh 05-touch-recording.html 05-touch-recording.png (+ "#en")
