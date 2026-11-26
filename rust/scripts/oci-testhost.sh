#!/usr/bin/env bash
# #18 / #20: run the daemonless OCI backend end-to-end on a Linux test host.
#
# Docker is used ONLY to provision the Linux test host (a privileged Debian container) and to
# export the pinned fixture images once. Inside the test host there is no Docker daemon, no
# containerd and no Python: OCI images run through `runc` driven by MVF, and Python comes from
# the image. Steps:
#   1. export pinned fixtures (busybox, python:3.12-slim) with `docker save`;
#   2. compile the Linux test binaries in rust:1.95-bookworm;
#   3. run them in debian:bookworm-slim + runc, privileged (namespaces, cgroups), with
#      MVF_REQUIRE_OCI=1 so no test can silently skip.
#   rust/scripts/oci-testhost.sh
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT="$REPO/rust/target/oci-testhost"
PLATFORM=${PLATFORM:-linux/arm64}
BUSYBOX=${BUSYBOX:-busybox:1.37@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec}
PYTHON=${PYTHON:-python:3.12-slim@sha256:191083cdf6c7bda8c76158983163ead5d5cf7b77cf3033e805bb0f5296f51b4b}
mkdir -p "$OUT/fixtures"

echo "== 1. fixtures ($PLATFORM)"
for pair in "busybox|$BUSYBOX" "python|$PYTHON"; do
  name=${pair%%|*}; ref=${pair#*|}
  if [[ ! -s "$OUT/fixtures/$name.tar" ]]; then
    docker pull -q --platform "$PLATFORM" "$ref" >/dev/null
    docker save --platform "$PLATFORM" "$ref" -o "$OUT/fixtures/$name.tar"
  fi
  echo "$name: $(du -h "$OUT/fixtures/$name.tar" | cut -f1) ($ref)"
done

echo "== 2. build Linux test binaries"
docker run --rm --platform "$PLATFORM" \
  -v "$REPO":/src -v mvf-cargo-registry:/usr/local/cargo/registry -v mvf-linux-target:/target \
  -e CARGO_TARGET_DIR=/target -w /src/rust rust:1.95-bookworm bash -ec '
    cargo test --locked -p mvf-container --test e2e --no-run 2>&1 | tail -2
    cargo build --locked -p mvf-cli 2>&1 | tail -1
    cp "$(ls -t /target/debug/deps/e2e-* | grep -v "\.d$" | head -1)" /target/mvf-oci-e2e'

echo "== 3. test host: debian:bookworm-slim + runc (no python, no docker daemon)"
docker run --rm --platform "$PLATFORM" --privileged --cgroupns=host --shm-size=512m \
  -v "$REPO":/src:ro -v mvf-linux-target:/target:ro -v "$OUT/fixtures":/fixtures:ro \
  -e MVF_OCI_E2E=1 -e MVF_REQUIRE_OCI=1 -e MVF_REQUIRE_NO_HOST_RUNTIME=1 -e MVF_OCI_FIXTURES=/fixtures \
  debian:bookworm-slim bash -ec '
    apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends runc >/dev/null
    echo "kernel: $(uname -srm)"; echo "cgroup: $(cat /sys/fs/cgroup/cgroup.controllers)"
    runc --version | head -3
    command -v python3 >/dev/null && { echo "python3 present on test host"; exit 1; } || echo "python3: absent (good)"
    /target/mvf-oci-e2e --test-threads=1 --nocapture 2>&1' | tee "$OUT/e2e.log"
grep -q "test result: ok" "$OUT/e2e.log"
