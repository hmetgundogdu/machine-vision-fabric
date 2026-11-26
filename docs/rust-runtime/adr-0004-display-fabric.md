# ADR-0004 — Display Fabric: presentation access as a runtime service

- **Status:** Accepted for the research branch (#30). Not a migration decision.
- **Date:** 2026-11-26
- **Issues:** #30, part of #10. Related: #23 (Network Fabric), #21 (event-driven execution), #27
  (Windows, rootless OCI).
- **Evidence:** [stage 18](stages/18.md) (P0 + P1), [stage 19](stages/19.md) (P2). Format: [`protocol/display-surface.md`](../../protocol/display-surface.md).

## Context

Inspection stations need pictures: a camera preview, a 3D view of a part, sometimes an existing
Qt/OpenCV/WPF application. MVF's edge runtime may run as a service with no desktop at all, and
must keep inspecting when nobody watches. Before #30 a node either drew nothing or opened windows
the runtime knew nothing about. Nothing negotiated, labelled or survived it.

## Decisions

1. **MVF manages presentation access and lifecycle; it never draws a node's UI.** There is no MVF
   GUI toolkit. A node that presents chooses one of three profiles in
   `permissions.display.mode`:
   - `none` (default): no display dependency.
   - `native`: the node's own toolkit opens its own windows. MVF decides whether it may run there
     (an interactive session) and supervises the process. Window and input are the application's.
   - `managed-surface`: the node renders into shared-memory surfaces, and a separate
     **display host** in the user's session presents them and routes back only granted input.

   The fourth profile, the contract-generated inspector, is P3 and is not part of this decision.
2. **The graph never waits for presentation.** The engine and scheduler are unchanged. There is no
   second scheduler, no display thread in the engine, no network service. A surface has three
   buffers, so presenting never blocks. With no host, a slow host or a crashed host, frames are
   simply not shown. Measured: a host killed mid-run cost the pipeline 0 of 300 frames.
3. **Session is part of negotiation.** `mvf_display::session::probe` classifies the engine's
   process:
   - *interactive*: Aqua; X11/Wayland with a reachable socket; a Windows session ≠ 0 with a
     visible window station;
   - *service*: Session 0, a launchd daemon or ssh;
   - *headless*: no display server.

   Workers inherit the engine's session, so `native` needs *interactive*. `managed-surface` works
   from any session. A required display that cannot be provided is rejected
   (`negotiation.display-unavailable`). `required: false` runs headless and says so
   (`placement.display-headless`). Clipboard and fullscreen are rejected, never dropped.
4. **One shared-memory file per window, language-neutral, no atomics required.**
   - Every field has one writer process.
   - Frames and input events carry a CRC-32 the reader verifies, so a producer without memory-order
     control (Python) is as safe as Rust: a torn frame is detected, not shown.
   - Exclusion is a file lock that the OS releases on death. A writer holds it for one present
     only; the host holds its own lock while it shows the surface.
   - Surfaces belong to the **pipeline node**, and the frame sequence lives in the surface. So a
     restarted, replacing or rolled-back generation simply continues the same window, with no
     engine hook (measured: v1 → v2 → v3 → rollback to v2 in one window with no gap).
5. **Least privilege for display access.**
   - Surfaces and descriptors live in a per-user 0700 rendezvous directory; an engine refuses a
     shared or foreign one.
   - The host takes the title, initial size and input grants only from the engine-written
     descriptor, never from the node.
   - The host validates every header and frame (bounds, format, CRC) as untrusted input.
   - Input is routed only where `keyboard` / `mouse` is granted.
   - Window events (focus, resize, close) inform the node and trigger nothing.
   - A node without a `native` grant does not inherit `DISPLAY`, `WAYLAND_DISPLAY` or `XAUTHORITY`.
     For trusted backends this is hygiene, not enforcement, and it is labelled as such.
6. **Containers get surfaces, never a display server.**
   - An OCI node's surfaces (and their writer locks) are bind-mounted into a private tmpfs at
     `/mvf/display`, owned by the mapped container root.
   - The run directory, the host lock, other nodes' surfaces, the X11 socket and the Wayland
     socket are not visible to it.
   - `native` is not granted to containers. X11 gives every client the whole display. A Wayland
     socket would give a node full compositor-client access, which is a decision for the
     operator: design-gated.
7. **Window libraries are the host's choice only.**
   - `mvf-display-host` uses winit 0.30 + softbuffer 0.4: CPU blit on Win32, AppKit, X11 and
     Wayland.
   - Node applications link nothing. The Python SDK is stdlib-only; the Rust renderer uses
     `mvf-display`, which has no window dependency.
   - wgpu is deliberately not used for presentation yet: GPU sharing is P4.

## Platform matrix (this host and CI, see stage 18)

| Combination | Native windows | Managed surfaces + host | Real input | Status |
|---|---|---|---|---|
| macOS, Aqua session | yes (115/120 frames drawn) | yes (2 windows, 0 rejected) | not injected (no synthetic input tool used) | **PASS** |
| macOS, daemon/ssh session | rejected (`service`) | node side yes; host needs Aqua | — | by design |
| Linux X11 (Xvfb) | yes | yes | **xdotool/XTEST**: granted node gets click/key/resize, ungranted only window events | **PASS** |
| Linux Wayland (weston 10, headless) | yes | yes | not injected | **PASS** |
| Linux headless (no server) | rejected | node side yes, host absent | — | **PASS** |
| Windows interactive session (CI `windows-latest`) | yes (native-window smoke passed) | yes (2 real windows) | not injected | **PASS** |
| Windows Session 0 service | rejected (`session 0`) | node side yes; host in the user session needs a cross-session ACL'd directory | — | **BLOCKED** (no service host available) |
| Linux OCI node (runc, no Docker daemon) | not granted (no display socket; Wayland design-gated) | yes: bind-mounted surface, denial, crash/restart, disconnect, cleanup | through the host, as for workers | **PASS** ([stage 19](stages/19.md)) |
| Windows, Linux OCI in WSL2 | WSLg's Wayland socket would carry native windows | engine and host both inside WSL2 is the Linux path; a host on the Windows side cannot map WSL2 files coherently (9p) | — | **BLOCKED** (no Windows/WSL2 machine) |
| macOS, Linux OCI in a VM (Lima, Virtualization.framework) | — | needs a guest → host transport (virtio-fs without DAX gives no shared pages; vsock copy) | — | **BLOCKED** (no VM in scope) |
| GPU shared textures (D3D, DMA-BUF, IOSurface) | — | — | — | P4, not started |

## Consequences

- **A developer writes business logic only.** The Python camera preview is 90 lines with
  `present()` and `poll_input()`. A click becomes typed JSON in the graph, never a machine action.
  A developer with their own toolkit keeps it (`native`).
- **Presentation cost is one copy into the surface plus one CRC, and one verified copy out.**
  At 1920×1080 BGRA on Apple M-series:
  - Rust present: 0.34 ms;
  - host copy-out: 0.23 ms;
  - Python present: 0.34 ms.

  The host polls every 2–4 ms, so present → copy latency is 1.6–5 ms p50.
- **Closing the window or crashing the host affects only presentation.** The node sees `closed` /
  `hostAttached`. Host attachment is detected at most 2 s after a crash (heartbeat staleness).
- **Not yet done:**
  - OCI presentation (part 2);
  - the inspector (P3);
  - GPU interop (P4);
  - Studio embedding (P5);
  - a doorbell to replace polling;
  - fullscreen and clipboard grants;
  - Wayland client-side decorations (windows have none under GNOME without the `sctk-adwaita`
    feature).

## Phases

1. P0 + P1, done ([stage 18](stages/18.md)):
   - capability matrix, session probe, negotiation;
   - native window PoC on macOS, Linux X11 and Wayland, and Windows CI;
   - display host on CPU shared memory;
   - Python camera preview and Rust interactive 3D renderer;
   - input, resize, close, multiple windows, host crash and reattach.
2. P2, done ([stage 19](stages/19.md)):
   - Linux OCI nodes presenting through bind-mounted surfaces, with denial, crash/restart,
     disconnect and cleanup tests;
   - live replacement and rollback keeping the window;
   - guest/host paths evaluated and marked BLOCKED.

   Open: Windows Session 0 → user-session host (BLOCKED, needs a cross-session ACL'd directory
   and a Windows service host); native windows for containers via an authorized Wayland socket
   (design-gated).
3. P3 inspector, P4 GPU sharing, P5 Studio embedding: design-gated, discuss first.

## Threat model

- **Protects against:**
  - a node reading another node's input (per-surface grants, refused at the host);
  - a node forging its window identity (descriptors are engine-written);
  - a malformed or torn frame crashing or confusing the host (validated, CRC);
  - stale surfaces of dead engines (swept);
  - a second host or a second generation tearing frames (file locks).
- **Does not protect against:**
  - a trusted (`process`) node that opens its own windows or reads the display socket anyway —
    it runs as the user, and only a sandbox (OCI, part 2) enforces display isolation;
  - other processes of the same user reading the 0700 directory;
  - screen capture by the window system;
  - a malicious display host run by the same user.
