#!/usr/bin/env bash
# #17 interoperability check between Docker and the MVF content store, both directions.
#
#   Docker -> MVF : `docker save` archive imported with `mvf image import` (no daemon involved:
#                   the import only reads the tar) and re-verified blob by blob.
#   MVF -> Docker : `mvf image export` as a Docker archive and as an OCI layout, each loaded with
#                   `docker load` and run with `docker run`; the loaded image id must equal the MVF
#                   manifest digest (containerd image store) — i.e. the identical image.
#
# Docker is optional test tooling here, never an MVF runtime dependency. Requires: a running
# Docker engine, network once to pull the pinned fixture.
#   rust/scripts/oci-interop.sh
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORK=${WORK:-"$REPO/rust/target/oci-interop"}
# Pinned fixture: busybox 1.37 image index (multi-platform); the platform is selected below.
IMAGE=${IMAGE:-busybox:1.37@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec}
NAME=${NAME:-docker.io/library/busybox:1.37}
PLATFORM=${PLATFORM:-linux/arm64}

(cd "$REPO/rust" && cargo build -q --locked -p mvf-cli)
MVF=("$REPO/rust/target/debug/mvf" image --store "$WORK/store")
rm -rf "$WORK" && mkdir -p "$WORK"

echo "== fixture: $IMAGE ($PLATFORM)"
docker pull -q --platform "$PLATFORM" "$IMAGE" >/dev/null
docker tag "$IMAGE" "$NAME"
docker save --platform "$PLATFORM" "$NAME" -o "$WORK/from-docker.tar"
echo "docker save archive entries: $(tar tf "$WORK/from-docker.tar" | tr '\n' ' ')"

echo "== Docker -> MVF"
"${MVF[@]}" import "$WORK/from-docker.tar" --platform "$PLATFORM" > "$WORK/record.json"
MANIFEST=$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["manifest"]["digest"]); sys.stderr.write("imported %s source=%s layers=%d\n" % (r["name"], r["source"], len(r["layers"])))' "$WORK/record.json")
"${MVF[@]}" verify "$NAME"
echo "manifest digest: $MANIFEST"

check_docker() { # archive label
  docker rmi -f "$NAME" >/dev/null 2>&1 || true
  docker load -q -i "$1" >/dev/null
  local id; id=$(docker image inspect "$NAME" --format '{{.Id}}')
  local out; out=$(docker run --rm --platform "$PLATFORM" "$NAME" echo "interop-$2-ok")
  echo "$2: docker image id $id; run says: $out"
  if [[ "$id" != "$MANIFEST" ]]; then
    echo "FAIL: Docker loaded a different image ($id != $MANIFEST)"; exit 1
  fi
}

echo "== MVF -> Docker"
"${MVF[@]}" export "$NAME" --format docker -o "$WORK/mvf-docker.tar"
check_docker "$WORK/mvf-docker.tar" docker-archive
"${MVF[@]}" export "$NAME" --format oci -o "$WORK/mvf-oci.tar"
check_docker "$WORK/mvf-oci.tar" oci-layout

echo "== MVF -> MVF (digest round trip)"
"${MVF[@]}" rm "$NAME"
"${MVF[@]}" import "$WORK/mvf-oci.tar" --platform "$PLATFORM" > "$WORK/record2.json"
python3 -c 'import json,sys; a=json.load(open(sys.argv[1])); b=json.load(open(sys.argv[2])); assert a["manifest"]==b["manifest"] and a["config"]==b["config"] and a["layers"]==b["layers"], "round trip changed a digest"; print("identical manifest/config/layers after export+import")' "$WORK/record.json" "$WORK/record2.json"
echo "PASS"
