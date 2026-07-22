#!/bin/bash
# Data-plane benchmark: push realistically-sized frames through the .NET->Python shared-memory path.
set -e
cd /Users/hmetgundogdu/Projects/machine-vision-fabric
CYCLES=${CYCLES:-2000}
FRAMES_DIR=packages/bench/assets/frames

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
  local out cycles dur cam bench fps gbps
  out=$(dotnet run --project src/cli/Mvf.Cli --no-build -- \
        execute-graph --path "$pipeline" --package packages/bench --max-cycles "$CYCLES" --no-tui 2>&1)
  cycles=$(echo "$out" | grep -oE 'cycles:[0-9]+'      | head -1 | cut -d: -f2)
  dur=$(echo "$out"    | grep -oE 'duration:[0-9.,]+'  | head -1 | cut -d: -f2 | tr ',' '.')
  cam=$(echo "$out"    | grep -E '^  cam:'   | grep -oE 'avg=[0-9.,]+' | cut -d= -f2 | tr ',' '.')
  bench=$(echo "$out"  | grep -E '^  bench:' | grep -oE 'avg=[0-9.,]+' | cut -d= -f2 | tr ',' '.')
  fps=$(awk  "BEGIN{printf \"%.0f\", $cycles/$dur}")
  gbps=$(awk "BEGIN{printf \"%.2f\", $cycles*$size/$dur/1e9}")
  local mb; mb=$(awk "BEGIN{printf \"%.2f\", $size/1048576}")
  printf "%-6s %7s MB | %5s f/s | %6s GB/s | src %6s ms | py %6s ms | %ss/%s cyc\n" \
         "$label" "$mb" "$fps" "$gbps" "$cam" "$bench" "$dur" "$cycles"
}

printf "== data-plane benchmark: %s cycles per row, 8MB slots, real python3 + numpy ==\n" "$CYCLES"
printf "%-6s %10s | %9s | %10s | %10s | %9s |\n" "mode" "frame" "throughput" "throughput" "src copy" "py rtt"
for size in 65536 1048576 2097152 6291456; do
  gen "$size"
  run packages/bench/pipeline-touch.json "$size" "touch"
  run packages/bench/pipeline-numpy.json "$size" "numpy"
done
