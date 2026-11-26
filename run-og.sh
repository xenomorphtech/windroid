#!/usr/bin/env bash
# Run Overgeared under wwd. usage: ./run-og.sh [seconds] [extra wwd flags...]
# seconds: kill the game after this long (default 0 = no limit).
# Needs the local servers reachable on 127.0.0.1:18080/10000/10002 (ssh tunnels
# to the overgeared host) for login.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
T=${1:-0}; shift || true
P=com.nexon.overgeared
# UI font repair (Korean text in NotoSansTC faces): scan for the UI fonts while
# the title screen loads; see crates/wwd-linux/src/fontfix.rs
export WWD_FONT_FIX=${WWD_FONT_FIX:-$(seq -s, 700 60 2300)}
timeout "${T}s" target/release/wwd.exe "$@" --root rootfs-android \
  --env LD_LIBRARY_PATH=/wwd/lib64:/data/app/$P/lib/arm64:/system/lib64 \
  rootfs-android/wwd/bin/wwd-app $P Temppal 2>&1 | grep --line-buffered -v "ld.config"
