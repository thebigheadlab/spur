#!/usr/bin/env bash
# Runs the scripted benchmark N times, then prints medians across runs, and (Linux) idle CPU.
# Linux needs a display (use: xvfb-run -a ./bench.sh). Usage: ./bench.sh [runs] [extra flags...]
# Every run counts (nothing is excluded); results/summary.txt is the table source for ADR 0002.
# Timings are input applied -> frame presented + GPU idle, vsync off. NOT key-to-photon.
set -euo pipefail
cd "$(dirname "$0")"
runs=${1:-5}; shift || true
bin=./target/release/spur-gui-spike
[[ -x $bin ]] || bin=./target/release/spur-gui-spike.exe
mkdir -p results
rm -f results/run*.json results/run*.err results/spawn.txt
now_ns() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time*1e9'; }
for i in $(seq 1 "$runs"); do
  start=$(now_ns)
  "$bin" --bench "$@" >"results/run$i.json" 2>"results/run$i.err"
  ff=$(sed -n 's/^first_frame_epoch_ns=//p' "results/run$i.err" | tr -d '\r')
  spawn=$(( (ff - start) / 1000000 ))
  echo "$spawn" >> results/spawn.txt
  echo "run $i: spawn->first frame $spawn ms"
done
# Median across runs of: spawn->first frame, per-phase p99, idle/end memory, skipped frames.
perl -e '
  sub med { my @v = sort { $a <=> $b } @_; return "n/a" unless @v; my $n=@v;
            return $n % 2 ? $v[$n/2] : ($v[$n/2-1]+$v[$n/2])/2 }
  my (%p99, @idle, @end, @skip, @spawn, $meta, $w);
  open my $s, "<", "results/spawn.txt" or die; chomp(@spawn = <$s>);
  for my $f (sort glob "results/run*.json") {
    local $/; open my $h, "<", $f or die; my $j = <$h>;
    ($meta) = $j =~ /"adapter": "([^"]*)"/ unless $meta;
    push @idle, $1 if $j =~ /"idle_mem_mb": ([\d.]+)/;
    push @end,  $1 if $j =~ /"end_mem_mb": ([\d.]+)/;
    push @skip, $1 if $j =~ /"skipped_frames": (\d+)/;
    while ($j =~ /"(\w+)": \{"n": \d+, "p50": [\d.]+, "p95": [\d.]+, "p99": ([\d.]+)/g) { push @{$p99{$1}}, $2 }
    if (!$w && $j =~ /"window": \[(\d+), (\d+)\], "scale": ([\d.]+)/) { $w = " | window ${1}x$2 scale $3"; $meta .= $w }
  }
  printf "runs: %d (median across runs; nothing excluded)\nadapter: %s\n", scalar(@spawn), $meta // "?";
  printf "spawn -> first frame: %s ms\n", med(@spawn);
  printf "%-12s p99 (input applied -> presented + GPU idle): %s ms\n", $_, med(@{$p99{$_}}) for sort keys %p99;
  printf "memory RSS after first frame: %s MB, at end: %s MB\nskipped (unpresented) frames: %s\n",
    med(@idle), med(@end), med(@skip);
' | tee results/summary.txt
if [[ $(uname) == Linux ]]; then
  "$bin" --idle-secs 12 "$@" & pid=$!
  sleep 3; a=$(awk '{print $14+$15}' /proc/$pid/stat)
  sleep 8; b=$(awk '{print $14+$15}' /proc/$pid/stat)
  echo "idle CPU over 8s: $(( (b - a) * 10 )) ms CPU time (clock ticks=$((b - a)))" | tee -a results/summary.txt
  wait $pid || true
fi
