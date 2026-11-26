#!/usr/bin/env bash
# #30: run the Display Fabric demo on this desktop (macOS, Linux with X11/Wayland).
#   rust/scripts/display-demo.sh [--max-cycles N]
# Builds release binaries, packages the Rust cube node, starts mvf-display-host in this session
# and runs rust/examples/display-demo. Ctrl+C stops both.
set -euo pipefail
RUST=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$RUST"
cargo build --release --locked -p mvf-display-host -p mvf-cli
MODULES="$RUST/target/display-demo/modules"
mkdir -p "$MODULES/rs.cube-inspector"
cp target/release/mvf-cube "$MODULES/rs.cube-inspector/mvf-cube"
cat > "$MODULES/rs.cube-inspector/node.yaml" <<'YAML'
apiVersion: mvf.dev/v1alpha1
kind: NodeContract
metadata: { id: rs.cube-inspector, version: 1.0.0, description: Camera frame on an interactive 3D cube (Display Fabric). }
spec:
  ports:
    inputs: [ { name: frame, channel: data, type: frame } ]
    outputs: [ { name: view, channel: data, type: json } ]
  artifacts: [ { backend: worker, runtime: executable, entry: mvf-cube, protocol: 1 } ]
  isolation: { minimum: process }
  permissions:
    display: { mode: managed-surface, required: false, mouse: true, keyboard: true, maxWidth: 1920, maxHeight: 1080 }
  presentation: { title: 3D inspector, width: 800, height: 600 }
YAML
target/release/mvf-display-host &
HOST=$!
trap 'kill $HOST 2>/dev/null || true' EXIT
BIN=target/release/mvf-rs
"$BIN" run examples/display-demo/pipeline.yaml --modules "$MODULES" --modules "$RUST/../modules" "$@"
