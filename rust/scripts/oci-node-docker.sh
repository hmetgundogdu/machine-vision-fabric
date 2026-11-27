#!/usr/bin/env bash
# #18: the MVF-packaged OCI node image also runs under standard Docker (Docker = optional test
# tooling, never an MVF runtime dependency). Docker cannot provide MVF graph semantics, so a tiny
# protocol harness plays the engine: it writes one typed frame into an arena file, bind-mounts it,
# sends `execute` over stdin and checks the inverted output the node wrote back into the arena.
#   rust/scripts/oci-node-docker.sh
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORK="$REPO/rust/target/oci-node-docker"
PLATFORM=${PLATFORM:-linux/arm64}
FIXTURE="$REPO/rust/target/oci-testhost/fixtures/python.tar"   # from oci-testhost.sh
(cd "$REPO/rust" && cargo build -q --locked -p mvf-cli)
MVF=("$REPO/rust/target/debug/mvf" image --store "$WORK/store")
rm -rf "$WORK" && mkdir -p "$WORK"

"${MVF[@]}" import "$FIXTURE" --name python:base --platform "$PLATFORM" >/dev/null
"${MVF[@]}" package --base python:base --name mvf/py-invert:docker \
  --module "$REPO/modules/py-invert-transformer" --entry transformer.py \
  --sdk "$REPO/src/sdk/python/mvf_sdk" --contract-out "$WORK/node.yaml"
"${MVF[@]}" export mvf/py-invert:docker --format docker -o "$WORK/node.tar"
docker rmi -f mvf/py-invert:docker >/dev/null 2>&1 || true
docker load -q -i "$WORK/node.tar"

# Arena: slot 0 = input frame [192-byte descriptor | 8 bytes], slot 1 (offset 4096) = output.
PYTHONPATH="$REPO/src/sdk/python" python3 - "$WORK/arena.bin" <<'PY'
import sys, mvf_sdk
buf = bytearray(8192)
mvf_sdk._write_descriptor(buf, 0, mvf_sdk.MediaType.IMAGE, mvf_sdk.ElementType.UINT8, (2, 4, 1), bytes(range(8)), 8)
open(sys.argv[1], "wb").write(buf)
PY
REPLY=$(printf '%s\n%s\n' \
  '{"type":"execute","id":1,"frame":{"cameraId":"cam","sequence":1,"contentType":"image/raw","shm":{"offset":0}},"out":{"offset":4096,"capacity":4096}}' \
  '{"type":"shutdown"}' \
  | docker run -i --rm --platform "$PLATFORM" --network none -e MVF_ARENA_PATH=/mvf/arena \
      -v "$WORK/arena.bin":/mvf/arena mvf/py-invert:docker)
echo "$REPLY"
PYTHONPATH="$REPO/src/sdk/python" python3 - "$WORK/arena.bin" <<'PY'
import sys, mvf_sdk
buf = open(sys.argv[1], "rb").read()
media, elem, shape, length = mvf_sdk._read_descriptor(buf, 4096)
out = buf[4096 + 192:4096 + 192 + length]
assert list(out) == [255 - b for b in range(8)], list(out)
print("docker-run node output verified: shape", shape, "bytes", list(out))
PY
echo "$REPLY" | grep -q '"type": "hello"' && echo "$REPLY" | grep -q '"type": "result"' && echo PASS
