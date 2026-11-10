#!/usr/bin/env bash
# Records a demo video of the app: the real app in demo mode (mock data, pinned clock, a clean
# backdrop), driven by scripts/demo/driver.swift, captured from the MacBook's own display.
#
#   scripts/record-demo.sh [out.mp4]      default: demo/notch-assistant.mp4
#
# Needs: ffmpeg, Xcode command-line tools (swiftc), and Screen Recording + Accessibility
# permission for your terminal. Keep hands off the mouse and keyboard while it runs.
set -euo pipefail
cd "$(dirname "$0")/.."

out=${1:-demo/notch-assistant.mp4}
work=$(mktemp -d)
trap 'kill "${app:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

cargo build --release --quiet
swiftc -O scripts/demo/driver.swift -o target/demo-driver

# One instance: stop a running copy (it's relaunched at the end if it was running).
was_running=$(pgrep -f target/release/notch-assistant >/dev/null && echo yes || echo no)
pkill -f target/release/notch-assistant || true
sleep 0.5

NOTCH_DEMO=1 target/release/notch-assistant >"$work/app.log" 2>&1 &
app=$!
sleep 3   # first frame, overlays prewarmed

index=$(target/demo-driver --display-index)
seconds=$(( $(target/demo-driver --duration) + 2 ))
echo "Recording display $index for ${seconds}s. Hands off the mouse and keyboard…"
screencapture -v -C -k -x -D"$index" -V"$seconds" "$work/raw.mov" &
recorder=$!
sleep 1
target/demo-driver
wait "$recorder"

kill "$app" 2>/dev/null || true
mkdir -p "$(dirname "$out")"
ffmpeg -loglevel error -y -i "$work/raw.mov" -ss 0.8 \
  -vf "scale=1920:-2:flags=lanczos,fps=60" -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p -movflags +faststart -an "$out"
echo "Wrote $out ($(du -h "$out" | cut -f1))"

if [ "$was_running" = yes ]; then
  nohup target/release/notch-assistant >/dev/null 2>&1 &
  echo "Relaunched the app."
fi
