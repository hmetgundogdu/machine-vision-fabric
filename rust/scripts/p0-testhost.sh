#!/usr/bin/env bash
# #20: the integrated P0 graph on the Linux test host, with every detector candidate an OCI image
# run by `runc` (no Docker daemon, no host Python). GPU nodes fall back to CPU there (the VM has
# no GPU), which the run reports as placement notes.
#   rust/scripts/p0-testhost.sh        (needs the fixtures from rust/scripts/oci-testhost.sh)
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT="$REPO/rust/target/p0-testhost"
FIXTURES="$REPO/rust/target/oci-testhost/fixtures"
PLATFORM=${PLATFORM:-linux/arm64}
mkdir -p "$OUT"
[[ -s "$FIXTURES/python.tar" ]] || { echo "run rust/scripts/oci-testhost.sh first (fixtures)"; exit 1; }

echo "== build mvf-rs (release, Linux)"
docker run --rm --platform "$PLATFORM" \
  -v "$REPO":/src -v mvf-cargo-registry:/usr/local/cargo/registry -v mvf-linux-target:/target \
  -e CARGO_TARGET_DIR=/target -w /src/rust rust:1.95-bookworm \
  bash -ec 'cargo build --release --locked -p mvf-cli 2>&1 | tail -1'

echo "== test host"
docker run --rm --platform "$PLATFORM" --privileged --cgroupns=host --shm-size=512m \
  -v "$REPO":/src:ro -v mvf-linux-target:/target:ro -v "$FIXTURES":/fixtures:ro -v "$OUT":/out \
  debian:bookworm-slim bash -ec '
    apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends runc time >/dev/null
    command -v python3 >/dev/null && { echo "python3 present on test host"; exit 1; } || echo "python3: absent"
    echo "kernel: $(uname -srm)"; runc --version | head -1
    MVF=/target/release/mvf-rs; export MVF_STORE=/root/.mvf/store
    W=/tmp/p0; mkdir -p $W/candidates; cp -r /src/rust/examples/p0/nodes /src/rust/examples/p0/pipeline-linux.yaml $W/
    $MVF image import /fixtures/python.tar --name python:base >/dev/null
    pkg() { $MVF image package --base python:base --name "mvf/p0-$1:1" --module "$2" --entry "$3" \
              --sdk /src/src/sdk/python/mvf_sdk --contract-out "$W/candidates/$1.yaml"; }
    pkg classifier /src/modules/py-brightness-classifier classifier.py
    pkg never-ready /src/rust/examples/p0/candidates/never-ready never_ready.py
    pkg crash /src/rust/examples/p0/candidates/crash-classifier crash.py
    cat > $W/updates.yaml <<EOF
apiVersion: mvf.dev/v1alpha1
kind: UpdatePlan
spec:
  updates:
    - { node: det, atCycle: 50, contract: candidates/classifier.yaml, probationCycles: 30 }
    - { node: det, atCycle: 700, contract: candidates/never-ready.yaml }
    - { node: det, atCycle: 1500, contract: candidates/crash.yaml, probationCycles: 300 }
EOF
    echo "oci state: /var/lib/mvf/oci (default for root)"
    /usr/bin/time -v $MVF run $W/pipeline-linux.yaml --updates $W/updates.yaml --require-oci \
      --verify --expect-promotions 2 --expect-rollbacks 1 --max-cutover-us 1000 \
      > /out/run.out 2> /out/run.err && code=0 || code=$?
    left=$(runc --root /var/lib/mvf/oci/runtime list -q 2>/dev/null | grep -c . || true)
    echo "containers left after run: $left"; [ "$left" = 0 ] || code=4
    echo "exit=$code"; exit $code' | tee "$OUT/host.log"
grep -vE '^\[|^cycle=' "$OUT/run.err" | grep -E 'ended|backend=|replace|data plane|cycle ms|verify|VERIFY|Maximum resident|Elapsed|User time|System time' | tee "$OUT/summary.txt"
grep -q 'VERIFY PASS' "$OUT/run.err"
