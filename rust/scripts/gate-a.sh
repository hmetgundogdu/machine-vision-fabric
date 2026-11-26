#!/usr/bin/env bash
# Gate A (issue #16): every required check plus the self-verifying foundation demo, one command.
#   rust/scripts/gate-a.sh            # from anywhere
# Prerequisites: the pinned Rust toolchain (rust-toolchain.toml), Python 3 on PATH (or MVF_PYTHON),
# optionally CMake + a C++ compiler for the C++ SDK interop test (MVF_REQUIRE_CPP=1 makes it mandatory).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
step() { printf '\n== %s\n' "$*"; }

step "fmt";    cargo fmt --all -- --check
step "clippy"; cargo clippy --workspace --all-targets --locked -- -D warnings
if command -v cmake >/dev/null; then
  step "C++ SDK example (interop fixture)"
  cmake -S ../src/sdk/cpp -B target/cpp-sdk -DCMAKE_BUILD_TYPE=Release >/dev/null
  cmake --build target/cpp-sdk --config Release >/dev/null
fi
step "tests";  cargo test --workspace --locked
step "demo: source -> live-replaced processor -> Python classifier -> sink, with rollback"
cargo run --release --locked -q -p mvf-cli -- run examples/live-update/pipeline.yaml \
  --updates examples/live-update/updates.yaml \
  --verify --expect-promotions 2 --expect-rollbacks 1 >/dev/null
step "Gate A checks passed"
