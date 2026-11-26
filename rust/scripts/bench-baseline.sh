#!/usr/bin/env bash
# Gate A baseline: the existing .NET engine vs the Rust runtime on matched workloads.
#
# Same frames (generated like packages/bench/bench.sh), same Python modules (py.bench-touch,
# py.bench-numpy), same topology (looping folder source -> Python classifier, no sink), serial
# executors, same cycle count. Both engines publish each frame into the shared-memory arena once
# and hand the worker an offset.
#
#   rust/scripts/bench-baseline.sh               # 2000 cycles per row
#   CYCLES=500 rust/scripts/bench-baseline.sh    # quicker
#   SIZES="65536 1048576" rust/scripts/bench-baseline.sh
#
# Columns:
#   engine    duration of the run as the engine reports it. .NET: ExecuteAsync (activation + cycles).
#             Rust: wallTimeMs (start + cycles + shutdown) — the closest matching scope.
#   f/s       cycles / engine duration
#   cycles    Rust only: time inside the cycle loop (excludes worker spawn/readiness)
#   node      average engine-side time of the Python node per cycle
#   process   wall time of the whole CLI process (includes runtime startup / CLR JIT)
#   rss       peak RSS from /usr/bin/time: the max over the host process and the worker processes it
#             reaped (getrusage semantics), not a sum. Comparable across engines, not a footprint total.
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$REPO"
CYCLES=${CYCLES:-2000}
SIZES=${SIZES:-"65536 1048576 6291456"}
FRAMES_DIR=packages/bench/assets/frames
OUT=${OUT:-rust/target/bench-baseline.txt}

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "Building .NET (Release) and Rust (release)..." >&2
  dotnet build Mvf.slnx -c Release -v q --nologo >/dev/null
  (cd rust && cargo build --release --locked -p mvf-cli >/dev/null 2>&1)
fi
MVF_RS="$REPO/rust/target/release/mvf-rs"
DOTNET_CLI=$(ls src/cli/Mvf.Cli/bin/Release/net*/Mvf.Cli.dll | head -1)

gen() {
  rm -rf "$FRAMES_DIR"; mkdir -p "$FRAMES_DIR"
  python3 -c "
buf = bytes([128]) * $1
for i in range(4):
    open('$FRAMES_DIR/f%d.bin' % i, 'wb').write(buf)
"
}

# Runs a command under /usr/bin/time; prints "<stdout+stderr of cmd>" and sets TIME_WALL / TIME_RSS_MB.
timed() {
  local tf; tf=$(mktemp)
  local out
  if [[ "$(uname)" == "Darwin" ]]; then
    out=$(/usr/bin/time -l "$@" 2>"$tf.err") || { cat "$tf.err" >&2; return 1; }
    TIME_RSS_MB=$(awk '/maximum resident set size/ {printf "%.0f", $1/1048576}' "$tf.err")
    TIME_WALL=$(awk '/ real / {print $1}' "$tf.err")
    cat "$tf.err" >> "$tf"
  else
    out=$(/usr/bin/time -v "$@" 2>"$tf.err") || { cat "$tf.err" >&2; return 1; }
    TIME_RSS_MB=$(awk -F: '/Maximum resident set size/ {printf "%.0f", $2/1024}' "$tf.err")
    TIME_WALL=$(awk -F': ' '/Elapsed \(wall clock\)/ {split($2,a,":"); if (length(a)==3) print a[1]*3600+a[2]*60+a[3]; else print a[1]*60+a[2]}' "$tf.err")
  fi
  RUN_OUT="$out
$(cat "$tf.err")"
  rm -f "$tf" "$tf.err"
}

row() { printf "%-5s %-6s %7s MB | %7s s | %6s f/s | %9s | %8s ms | %7s s | %5s MB\n" "$@"; }

{
printf "== Gate A baseline: %s cycles/row, serial, %s, %s ==\n" "$CYCLES" "$(uname -sm)" "$(date -u +%Y-%m-%dT%H:%MZ)"
printf "python %s, numpy %s, dotnet %s, %s\n" \
  "$(python3 -c 'import platform; print(platform.python_version())')" \
  "$(python3 -c 'import numpy; print(numpy.__version__)' 2>/dev/null || echo n/a)" \
  "$(dotnet --version)" "$("$MVF_RS" --version)"
row "eng" "mode" "frame" "engine" "rate" "cycles" "node" "process" "rss"
for size in $SIZES; do
  gen "$size"
  mb=$(awk "BEGIN{printf \"%.2f\", $size/1048576}")
  slot=$(( (size + 192 + 63) / 64 * 64 ))
  for mode in touch numpy; do
    # .NET (existing engine, unchanged)
    timed dotnet "$DOTNET_CLI" execute-graph --path "packages/bench/pipeline-$mode.json" \
      --package packages/bench --max-cycles "$CYCLES" --mode serial --no-tui
    echo "$RUN_OUT" | grep -q 'Succeeded:True' || { echo ".NET run failed"; echo "$RUN_OUT" | tail -5; exit 1; }
    dur=$(echo "$RUN_OUT" | grep -oE 'duration:[0-9.,]+' | head -1 | cut -d: -f2 | tr ',' '.')
    node=$(echo "$RUN_OUT" | grep -E '^  bench:' | grep -oE 'avg=[0-9.,]+' | cut -d= -f2 | tr ',' '.')
    fps=$(awk "BEGIN{printf \"%.0f\", $CYCLES/$dur}")
    row ".NET" "$mode" "$mb" "$dur" "$fps" "-" "$node" "$TIME_WALL" "$TIME_RSS_MB"

    # Rust (research runtime)
    timed "$MVF_RS" run "packages/bench/rust-pipeline-$mode.yaml" --max-cycles "$CYCLES" --json \
      --arena-slot-size "$slot" --arena-slots 4
    report=$(echo "$RUN_OUT" | grep '^{"report"')
    read -r wall cyc node_us executed < <(echo "$report" | python3 -c '
import json, sys
r = json.load(sys.stdin)["report"]
assert r["end"]["reason"] == "cycle-limit", r["end"]
b = [n for n in r["nodes"] if n["id"] == "bench"][0]
print(r["wallTimeMs"] / 1e3, r["cyclesMs"] / 1e3, b["totalUs"], b["executed"])')
    rnode=$(awk "BEGIN{printf \"%.3f\", $node_us/$executed/1000}")
    rfps=$(awk "BEGIN{printf \"%.0f\", $CYCLES/$wall}")
    row "Rust" "$mode" "$mb" "$(printf %.2f "$wall")" "$rfps" "$(printf %.2fs "$cyc")" "$rnode" "$TIME_WALL" "$TIME_RSS_MB"
  done
done
} | tee "$OUT"
echo "saved to $OUT" >&2
