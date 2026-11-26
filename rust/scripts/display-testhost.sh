#!/usr/bin/env bash
# #30: Display Fabric on Linux, with real display servers and real OS input.
#
# Docker only provisions a Linux box (rust:1.95-bookworm). Inside it:
#   1. headless (no DISPLAY / WAYLAND_DISPLAY): session probe, surfaces, pipelines, host crash;
#   2. X11 under Xvfb: real windows from mvf-display-host and from a native-UI node, and real input
#      injected with xdotool (XTEST) that must reach only the node granted it;
#   3. Wayland under a headless weston compositor: the same windows through wl_shm.
#   rust/scripts/display-testhost.sh
set -euo pipefail
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT="$REPO/rust/target/display-testhost"
PLATFORM=${PLATFORM:-linux/arm64}
mkdir -p "$OUT"

docker run --rm --platform "$PLATFORM" --shm-size=256m \
  -v "$REPO":/src -v mvf-cargo-registry:/usr/local/cargo/registry -v mvf-linux-target:/target \
  -e CARGO_TARGET_DIR=/target -e PYTHONDONTWRITEBYTECODE=1 -w /src/rust rust:1.95-bookworm bash -ec '
    apt-get update -qq >/dev/null
    apt-get install -y -qq --no-install-recommends xvfb xdotool weston \
      libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libxkbcommon-x11-0 \
      libwayland-client0 libwayland-cursor0 >/dev/null
    echo "kernel: $(uname -srm)"; dpkg-query -W xdotool xvfb weston
    cargo test --locked -p mvf-display -p mvf-display-host -p mvf-worker --no-run 2>&1 | tail -1
    probe() { "$CARGO_TARGET_DIR/debug/mvf-display-host" --probe; }

    echo "== 1. headless"
    unset DISPLAY WAYLAND_DISPLAY
    probe
    cargo test --locked -p mvf-display -p mvf-display-host 2>&1 | grep -E "^test |test result|BLOCKED|skipped"
    cargo test --locked -p mvf-worker --test display 2>&1 | grep -E "^test |test result"

    echo "== 2. X11 (Xvfb) + xdotool"
    Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
    sleep 1
    DISPLAY=:99 probe
    DISPLAY=:99 MVF_DISPLAY_SMOKE=1 MVF_DISPLAY_XDOTOOL=1 \
      cargo test --locked -p mvf-display-host --test host -- --test-threads 1 --nocapture 2>&1 \
      | grep -E "^test |test result|panicked|real window|native window|window opened|granted:|denied:|host stats|BLOCKED|cube:|host crash"

    echo "== 3. Wayland (weston, headless backend)"
    export XDG_RUNTIME_DIR=/tmp/xdg && mkdir -p -m 700 "$XDG_RUNTIME_DIR"
    weston --backend=headless-backend.so --socket=wayland-mvf --idle-time=0 >/tmp/weston.log 2>&1 &
    sleep 2
    if [ -S "$XDG_RUNTIME_DIR/wayland-mvf" ]; then
      WAYLAND_DISPLAY=wayland-mvf probe
      WAYLAND_DISPLAY=wayland-mvf MVF_DISPLAY_SMOKE=1 \
        cargo test --locked -p mvf-display-host --test host -- --test-threads 1 --nocapture a_real_window a_native_ui 2>&1 \
        | grep -E "^test |test result|panicked|real window|native window|window opened|BLOCKED"
    else
      echo "BLOCKED: weston did not start"; tail -5 /tmp/weston.log
    fi
  ' 2>&1 | tee "$OUT/display.log"
grep -q "test result: ok" "$OUT/display.log" && ! grep -q "FAILED\|panicked" "$OUT/display.log"
