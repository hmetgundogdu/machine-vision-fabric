#!/usr/bin/env bash
# #32 part 2: the C++ and .NET worker SDKs on a Linux host, from macOS or any Docker host.
#
# Docker only provides the Linux machine (rust:1.95-bookworm, which has Python for the Python
# nodes). Steps:
#   1. publish the .NET examples self-contained for the container's architecture (needs `dotnet`
#      on this machine; the container then needs no .NET runtime);
#   2. inside the container, build the C++ SDK and its examples with g++ (the nlohmann/json header
#      comes from a host `cmake` configure of src/sdk/cpp into rust/target/cpp-sdk);
#   3. run crates/mvf-worker/tests/sdks.rs with both SDKs required, and with BENCH=1 the
#      through-the-engine benchmark (crates/mvf-ipc/tests/engine_bench.rs).
#
#   rust/scripts/worker-sdks-linux.sh            # tests
#   BENCH=1 rust/scripts/worker-sdks-linux.sh    # tests + benchmark
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PLATFORM=${PLATFORM:-linux/arm64}
RID=linux-${PLATFORM#linux/}
RID=${RID/amd64/x64}
JSON="$REPO/rust/target/cpp-sdk/_deps/nlohmann_json-src/include"
[[ -d "$JSON" ]] || { echo "missing $JSON: run cmake -S src/sdk/cpp -B rust/target/cpp-sdk first"; exit 1; }

echo "== 1. .NET examples ($RID, self-contained)"
for p in InvertTransformer MeanAnalyzer; do
  dotnet publish "$REPO/src/sdk/dotnet/examples/$p" -c Release -r "$RID" --self-contained \
    -o "$REPO/rust/target/dotnet-sdk-linux/$p" --nologo -v q
done

echo "== 2+3. C++ build, SDK suite${BENCH:+ and benchmark} ($PLATFORM)"
docker run --rm --platform "$PLATFORM" \
  -v "$REPO":/src -v mvf-cargo-registry:/usr/local/cargo/registry -v mvf-linux-target:/target \
  -e CARGO_TARGET_DIR=/target -e PYTHONDONTWRITEBYTECODE=1 \
  -e MVF_CPP_SDK_DIR=/target/cpp-sdk-linux -e MVF_DOTNET_SDK_DIR=/src/rust/target/dotnet-sdk-linux \
  -e MVF_REQUIRE_CPP=1 -e MVF_REQUIRE_DOTNET=1 -e BENCH="${BENCH:-}" \
  -w /src/rust rust:1.95-bookworm bash -ec '
    echo "kernel: $(uname -srm), $(nproc) CPUs"
    out=$MVF_CPP_SDK_DIR; mkdir -p "$out"
    inc="-I/src/src/sdk/cpp/include -I/src/rust/target/cpp-sdk/_deps/nlohmann_json-src/include"
    g++ -O2 -std=c++17 -fPIC -shared $inc /src/src/sdk/cpp/src/sdk.cpp -o "$out/libmvf_sdk.so"
    for e in invert_transformer mean_analyzer; do
      g++ -O2 -std=c++17 $inc "/src/src/sdk/cpp/examples/$e.cpp" -L"$out" -lmvf_sdk \
        -Wl,-rpath,"\$ORIGIN" -o "$out/mvf_$e"
    done
    cargo test --release --locked -p mvf-worker --test sdks -- --test-threads 1 2>&1 | tail -3
    if [[ -n "$BENCH" ]]; then
      MVF_IPC_BENCH=1 cargo test --release --locked -p mvf-ipc --test engine_bench -- --nocapture \
        --test-threads 1 2>&1 | grep -E "^node|transformer|analyzer|NOT RUN|test result"
    fi'
