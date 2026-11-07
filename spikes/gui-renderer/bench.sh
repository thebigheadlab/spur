#!/usr/bin/env bash
# Runs the scripted benchmark N times and measures idle CPU. Linux needs a display
# (use: xvfb-run -a ./bench.sh). Usage: ./bench.sh [runs] [extra flags...]
set -euo pipefail
cd "$(dirname "$0")"
runs=${1:-5}; shift || true
bin=./target/release/spur-gui-spike
[[ -x $bin ]] || bin=./target/release/spur-gui-spike.exe
mkdir -p results
now_ns() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time*1e9'; }
for i in $(seq 1 "$runs"); do
  start=$(now_ns)
  "$bin" --bench "$@" >"results/run$i.json" 2>"results/run$i.err"
  ff=$(sed -n 's/^first_frame_epoch_ns=//p' "results/run$i.err" | tr -d '\r')
  echo "run $i: spawn->first frame $(( (ff - start) / 1000000 )) ms"
done
if [[ $(uname) == Linux ]]; then
  "$bin" --idle-secs 12 "$@" & pid=$!
  sleep 3; a=$(awk '{print $14+$15}' /proc/$pid/stat)
  sleep 8; b=$(awk '{print $14+$15}' /proc/$pid/stat)
  echo "idle CPU over 8s: $(( (b - a) * 10 )) ms CPU time (clock ticks=$((b - a)))"
  wait $pid || true
fi
