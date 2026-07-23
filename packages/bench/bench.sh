#!/bin/bash
# Data-plane benchmark: push realistically-sized frames through the .NET -> Python shared-memory path.
#
# Self-contained: locates the repo from its own path, generates its own frames, and builds unless
# SKIP_BUILD=1. Nothing here needs hardware — the source is the folder simulator (Simulator-First).
#
#   ./packages/bench/bench.sh              # default 2000 cycles per row
#   CYCLES=200 ./packages/bench/bench.sh   # quick pass
#
# Read it as the *serial executor's* baseline: one frame in flight at a time, so throughput is bounded by
# the sum of the stage latencies. That is exactly what the pipelined executor is meant to change, so
# record a fresh run before and after (see README.md).
set -e

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$REPO_ROOT"

CYCLES=${CYCLES:-2000}
FRAMES_DIR=packages/bench/assets/frames

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "Building (SKIP_BUILD=1 to skip)..."
  dotnet build -v q --nologo >/dev/null
fi

gen() { # size_bytes
  rm -rf "$FRAMES_DIR"; mkdir -p "$FRAMES_DIR"
  python3 -c "
size=$1
buf=bytes([128])*size
for i in range(4):
    open('$FRAMES_DIR/f%d.bin'%i,'wb').write(buf)
"
}

run() { # pipeline_file  size_bytes  label
  local pipeline=$1 size=$2 label=$3
  local out cycles dur node rpc restarts fps gbps
  out=$(dotnet run --project src/cli/Mvf.Cli --no-build -- \
        execute-graph --path "$pipeline" --package packages/bench --max-cycles "$CYCLES" --no-tui 2>&1) || true

  # A benchmark must never quote numbers from a run that failed or that lost a worker mid-flight.
  if ! echo "$out" | grep -q 'Succeeded:True'; then
    echo "FAILED ($label, $size bytes):"; echo "$out" | tail -5; exit 1
  fi
  restarts=$(echo "$out" | grep -oE 'restarts:[0-9]+' | head -1 | cut -d: -f2)
  if [[ "${restarts:-0}" != "0" ]]; then
    echo "FAILED ($label): $restarts worker restart(s) during the run — numbers would be meaningless."; exit 1
  fi

  cycles=$(echo "$out" | grep -oE 'cycles:[0-9]+'     | head -1 | cut -d: -f2)
  dur=$(echo "$out"    | grep -oE 'duration:[0-9.,]+' | head -1 | cut -d: -f2 | tr ',' '.')
  # Node average = engine-side cost of the whole node; rpc average = the worker round-trip inside it
  # (reported per worker since the cross-process observability slice). The gap is engine overhead.
  node=$(echo "$out"   | grep -E '^  bench:'   | grep -oE 'avg=[0-9.,]+' | cut -d= -f2 | tr ',' '.')
  rpc=$(echo "$out"    | grep -E '^ +worker=' | grep -oE 'avg=[0-9.,]+' | cut -d= -f2 | tr ',' '.')

  # Parenthesised: bare `>` after printf's argument list is parsed as output redirection, not comparison.
  fps=$(awk  "BEGIN{printf \"%.0f\", ($dur>0 ? $cycles/$dur : 0)}")
  gbps=$(awk "BEGIN{printf \"%.2f\", ($dur>0 ? $cycles*$size/$dur/1e9 : 0)}")
  local mb; mb=$(awk "BEGIN{printf \"%.2f\", $size/1048576}")
  printf "%-6s %7s MB | %5s f/s | %6s GB/s | node %6s ms | rpc %6s ms | %ss/%s cyc\n" \
         "$label" "$mb" "$fps" "$gbps" "$node" "$rpc" "$dur" "$cycles"
}

printf "== data-plane benchmark: %s cycles per row, real python3 + numpy, serial executor ==\n" "$CYCLES"
printf "%-6s %10s | %9s | %10s | %11s | %10s |\n" "mode" "frame" "throughput" "throughput" "node" "py rtt"
for size in 65536 1048576 2097152 6291456; do
  gen "$size"
  run packages/bench/pipeline-touch.json "$size" "touch"
  run packages/bench/pipeline-numpy.json "$size" "numpy"
done
