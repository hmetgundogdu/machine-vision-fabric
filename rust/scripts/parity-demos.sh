#!/usr/bin/env bash
# Output parity of the demo packages (issue #25): the .NET engine runs packages/<demo>, the Rust
# runtime runs its twin rust/examples/<demo>; same frames, same Python modules, same cycle count,
# pacing switched off on both sides. Every file each dataset writer produced is compared by name and
# SHA-256, plus the per-frame metadata that is not a path or a timestamp.
#
#   rust/scripts/parity-demos.sh                 # 24 cycles per demo
#   CYCLES=40 SKIP_BUILD=1 rust/scripts/parity-demos.sh
#   RUST_MODE=pipelined rust/scripts/parity-demos.sh   # the Rust twins stage-parallel
#   RUST_SOURCE=import rust/scripts/parity-demos.sh    # convert each legacy package with
#                                                     # `mvf import` instead of using the twin (#34)
#
# value-demo: the .NET CLI exposes no selection output, so its dataset is compared like the others
# and the Rust selections are checked against what the .NET reference tests assert
# (ValueAndSelectGraphTests: camera DEF456 picked by binding; protocol `gige` keeps ABC123 + DEF456).
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CYCLES=${CYCLES:-24}
RUST_MODE=${RUST_MODE:-serial}
RUST_SOURCE=${RUST_SOURCE:-twin}
export RUST_SOURCE
DEMOS=${DEMOS:-"inspection-demo loop-demo value-demo"}
WORK=${WORK:-$(mktemp -d "${TMPDIR:-/tmp}/mvf-parity.XXXXXX")}
export MVF_BINDING_CAMERA_SERIAL=DEF456

# The legacy .NET engine is archived (#34). LEGACY_ROOT is a checkout of it: by default a worktree of
# the tag `dotnet-legacy-final` at <repo>/.legacy-dotnet (git-ignored), made and built on first use.
LEGACY_ROOT=${LEGACY_ROOT:-$REPO/.legacy-dotnet}
if [[ ! -d "$LEGACY_ROOT" ]]; then
  git -C "$REPO" rev-parse -q --verify dotnet-legacy-final >/dev/null \
    || git -C "$REPO" fetch -q origin tag dotnet-legacy-final
  git -C "$REPO" worktree add -q --detach "$LEGACY_ROOT" dotnet-legacy-final
fi
if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "Building the legacy .NET engine (Release) and mvf (release)..." >&2
  (cd "$LEGACY_ROOT" && dotnet build Mvf.slnx -c Release -v q --nologo >/dev/null)
  (cd "$REPO/rust" && cargo build --release --locked -p mvf-cli >/dev/null)
fi
DOTNET_CLI=$(ls "$LEGACY_ROOT"/src/cli/Mvf.Cli/bin/Release/net*/Mvf.Cli.dll | head -1)
MVF_RS="$REPO/rust/target/release/mvf"

mkdir -p "$WORK/dotnet" "$WORK/rust/packages" "$WORK/rust/rust/examples"
cp -R "$REPO/packages/multilang-demo" "$WORK/dotnet/"
cp -R "$REPO/packages/multilang-demo" "$WORK/rust/packages/"

for demo in $DEMOS; do
  # .NET: a copy of the package with pacing off.
  rm -rf "$WORK/dotnet/$demo"
  cp -R "$REPO/packages/$demo" "$WORK/dotnet/$demo"
  rm -rf "$WORK/dotnet/$demo/out" "$WORK/dotnet/$demo/.mvf"
  python3 -I - "$WORK/dotnet/$demo/pipeline.json" <<'PY'
import json, sys
path = sys.argv[1]
doc = json.load(open(path))
for node in doc["nodes"]:
    if "frameIntervalMs" in node.get("config", {}):
        node["config"]["frameIntervalMs"] = 0
json.dump(doc, open(path, "w"), indent=1)
PY
  out=$(cd "$WORK/dotnet" && dotnet "$DOTNET_CLI" execute-graph --package "$WORK/dotnet/$demo" \
    --no-tui --no-prompt --max-cycles "$CYCLES" --integrations-root "$LEGACY_ROOT/modules" 2>&1)
  echo "$out" | grep -q 'Succeeded:True' || { echo ".NET $demo failed:"; echo "$out" | tail -5; exit 1; }

  if [[ "$RUST_SOURCE" == "import" ]]; then
    # Rust: the legacy package itself (pacing off, as for .NET), converted by `mvf import`.
    rm -rf "$WORK/rust/packages/$demo"
    cp -R "$WORK/dotnet/$demo" "$WORK/rust/packages/$demo"
    rm -rf "$WORK/rust/packages/$demo/out" "$WORK/rust/packages/$demo/.mvf"
    "$MVF_RS" import "$WORK/rust/packages/$demo" --modules "$REPO/modules" --force \
      > "$WORK/rust/$demo.import" 2>&1 || true
    grep -q '^wrote ' "$WORK/rust/$demo.import" || { echo "import $demo failed:"; tail -5 "$WORK/rust/$demo.import"; exit 1; }
    RUST_PIPELINE="$WORK/rust/packages/$demo/pipeline.yaml"
  else
    # Rust: the twin, laid out so its relative frame folder resolves inside the work dir.
    rm -rf "$WORK/rust/rust/examples/$demo"
    mkdir -p "$WORK/rust/rust/examples/$demo"
    sed -E 's/frameIntervalMs: [0-9]+/frameIntervalMs: 0/' "$REPO/rust/examples/$demo/pipeline.yaml" \
      > "$WORK/rust/rust/examples/$demo/pipeline.yaml"
    RUST_PIPELINE="$WORK/rust/rust/examples/$demo/pipeline.yaml"
  fi
  "$MVF_RS" run "$RUST_PIPELINE" --modules "$REPO/modules" \
    --max-cycles "$CYCLES" --mode "$RUST_MODE" --json > "$WORK/rust/$demo.jsonl" 2> "$WORK/rust/$demo.err" \
    || { echo "Rust $demo failed:"; tail -5 "$WORK/rust/$demo.err"; exit 1; }
done

python3 -I - "$WORK" "$CYCLES" $DEMOS <<'PY'
import hashlib, json, os, re, sys
work, cycles, demos = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
KEEP = ("cameraId", "sequenceNumber", "sourcePath", "contentType", "contentLength")

def collect(root):
    """{logical path: sha256} for every dataset file; session dirs lose their timestamp."""
    files, meta = {}, {}
    for base, _, names in os.walk(root):
        for name in names:
            full = os.path.join(base, name)
            rel = os.path.relpath(full, root)
            rel = re.sub(r"([A-Za-z]+)-\d{8}-\d{6}-\d{3}", r"\1", rel)
            if rel.endswith("session.json"):
                continue  # paths and timestamps only
            if "/metadata/" in "/" + rel:
                record = json.load(open(full))
                meta[rel] = {k: record.get(k) for k in KEEP}
            else:
                files[rel] = hashlib.sha256(open(full, "rb").read()).hexdigest()
    return files, meta

ok = True
print(f"{'demo':<16} {'.NET files':>10} {'Rust files':>10}  result")
for demo in demos:
    dn, dm = collect(os.path.join(work, "dotnet", demo, "out"))
    imported = os.environ.get("RUST_SOURCE") == "import"
    rust_dir = ("packages",) if imported else ("rust", "examples")
    rn, rm = collect(os.path.join(work, "rust", *rust_dir, demo, "out"))
    same = dn == rn and dm == rm and len(dn) > 0
    detail = ""
    if not same:
        ok = False
        missing = sorted(set(dn) - set(rn))[:3]
        extra = sorted(set(rn) - set(dn))[:3]
        differ = sorted(k for k in set(dn) & set(rn) if dn[k] != rn[k])[:3]
        mdiff = sorted(k for k in set(dm) | set(rm) if dm.get(k) != rm.get(k))[:3]
        detail = f" missing={missing} extra={extra} differ={differ} metadata={mdiff}"
    print(f"{demo:<16} {len(dn):>10} {len(rn):>10}  {'IDENTICAL' if same else 'DIFFERENT'}{detail}")
    lines = [json.loads(l) for l in open(os.path.join(work, "rust", f"{demo}.jsonl")) if l.startswith("{")]
    report = next(l["report"] for l in lines if "report" in l)
    # A demo without a loop ends when its folder runs dry, on both runtimes (same file sets above).
    if report["end"] != {"reason": "cycle-limit", "limit": cycles} and report["end"].get("reason") != "source-exhausted":
        ok = False
        print(f"  Rust run ended with {report['end']}")
    if demo == "value-demo" and imported:
        print("  selections: not checked (the twin's recording sinks are not in the legacy package)")
    elif demo == "value-demo":
        records = [l["record"] for l in lines if "record" in l]
        chosen = {json.dumps(r["value"]["value"], sort_keys=True) for r in records if r["node"] == "chosen"}
        narrowed = {json.dumps(r["value"]["value"], sort_keys=True) for r in records if r["node"] == "narrowed"}
        want_one = {json.dumps({"serial": "DEF456", "protocol": "gige", "address": "10.0.0.5"}, sort_keys=True)}
        want_many = {json.dumps([{"serial": "ABC123", "protocol": "gige", "address": "10.0.0.4"},
                                 {"serial": "DEF456", "protocol": "gige", "address": "10.0.0.5"}], sort_keys=True)}
        sel = chosen == want_one and narrowed == want_many
        n_one = sum(1 for r in records if r["node"] == "chosen")
        print(f"  selections: pick-one={'DEF456' if chosen == want_one else chosen} "
              f"pick-protocol={'[ABC123, DEF456]' if narrowed == want_many else narrowed} "
              f"every cycle={n_one == cycles} -> {'MATCH' if sel and n_one == cycles else 'MISMATCH'}")
        ok = ok and sel and n_one == cycles
print("PARITY", "PASS" if ok else "FAIL", f"({cycles} cycles per demo, Rust mode {os.environ.get('RUST_MODE', 'serial')}, Rust pipelines from {os.environ.get('RUST_SOURCE', 'twin')}, work dir {work})")
sys.exit(0 if ok else 1)
PY
