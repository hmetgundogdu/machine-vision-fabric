# MVF display surface, format v1 (#30)

A **managed surface** is one shared-memory file per window. A node (writer) renders frames into
it; a display host (reader) in the user's session copies the newest frame out and shows it, and
writes back status and the input the node's contract grants. Local only: no socket, no network,
no window library in the node. Reference implementations: `rust/crates/mvf-display`
(`SurfaceWriter`, `SurfaceReader`) and `src/sdk/python/mvf_sdk/display.py`.

## Rules

- **One writer per field.** Every field below has exactly one writing process, so nothing needs
  compare-and-swap. A producer without atomics (Python) speaks the format.
- **Integrity by checksum, not by memory ordering.** Each frame record and each input event
  carries a CRC-32 (IEEE, as zlib's `crc32`). The reader verifies it and skips what does not
  verify. A torn or not-yet-visible frame is never shown.
- **Never block.** With three buffers the writer always has one that is neither published nor
  being copied. The input ring drops events when full; it never waits.
- **Untrusted peers.** The host validates the header geometry, every frame's size/stride/format
  and the CRC before touching pixels. The node validates every event's CRC.
- **Ownership by file lock.** The writing generation holds an exclusive lock on
  `<surface>.writer.lock`, the presenting host on `<surface>.host.lock` (`flock` on Unix,
  `LockFileEx` / `msvcrt.locking` on Windows). The OS releases both when a process dies. A
  successor generation that cannot take the writer lock is on *standby*: `present` writes
  nothing and returns at once. It retries at most every 50 ms.

All integers are little-endian.

## Layout

| Offset | Size | Writer | Field |
|---|---|---|---|
| 0 | 8 | engine | magic `MVFSURF1` |
| 8 | 4 | engine | version = 1 |
| 12 | 4 | engine | buffer count = 3 |
| 16 / 20 | 4 / 4 | engine | max width / max height (1..=8192) |
| 24 | 8 | engine | buffer capacity = align4096(max_w × max_h × 4) |
| 32 | 8 | engine | buffers offset = align4096(4096 + ring slots × 64) |
| 40 | 8 | engine | ring offset = 4096 |
| 48 / 52 | 4 / 4 | engine | ring slots = 256 / slot size = 64 |
| 56 | 4 | engine | window index within the node |
| 60 | 4 | engine | CRC-32 of bytes 0..60 |
| 256 | 8 | node | **published** = `seq << 2 \| buffer index`; 0 = nothing yet |
| 264 | 8 | node | writer pid (0 when none) |
| 272 | 8 | node | frames presented |
| 280 | 8 | node | input **tail**: events consumed |
| 512 | 8 | host | **reading** = buffer index + 1 being copied; 0 = none |
| 520 | 8 | host | host pid |
| 528 | 8 | host | input **head**: events written |
| 1024 | 52 + 4 | host | status (below) + CRC-32 of its 52 bytes |
| 2048 + 64·i | 40 + 4 | node | frame record of buffer *i* + CRC-32 |
| 4096 | 256 × 64 | host | input ring |
| buffers offset + i × capacity | capacity | node | frame buffer *i* |

**Frame record** (40 bytes): `seq u64`, `width u32`, `height u32`, `stride u32`, `format u32`,
`timestamp_us u64` (Unix µs at present), `len u64` (= stride × height). The CRC-32 covers the 40
record bytes followed by the `len` pixel bytes.

Formats: 1 `bgra8`, 2 `rgba8`, 3 `gray8`, 4 `rgb8`, 5 `bgr8`.

**Status** (52 bytes): `counter u64` (increments on every write; 0 = never attached),
`heartbeat_ms u64`, `flags u32` (1 attached, 2 focused, 4 closed by the user, 8 visible),
`requested_width u32`, `requested_height u32` (the window's content size in physical pixels),
`scale_milli u32` (DPI scale × 1000), `shown u64`, `dropped_events u64`, `host_pid u32`. A node
treats the host as present while the attached flag is set and the heartbeat is under 2 s old.

**Input event** (64 bytes): `seq u64` (event number, from 1), `kind u16`, `modifiers u16`
(1 shift, 2 ctrl, 4 alt, 8 super), `code u32`, `x f32`, `y f32`, `dx f32`, `dy f32`,
`text [24]` (UTF-8, NUL-padded), CRC-32 of bytes 0..56, 4 reserved.

| kind | name | needs grant | fields |
|---|---|---|---|
| 1 | pointer-move | `mouse` | x, y in **frame** pixels |
| 2 / 3 | pointer-down / pointer-up | `mouse` | code = button (0 left, 1 right, 2 middle), x, y |
| 4 | wheel | `mouse` | dx, dy in lines |
| 5 / 6 | key-down / key-up | `keyboard` | text = logical key (`a`, `Enter`, `ArrowLeft`), code 1 = repeat |
| 7 | focus | — | code 1 = gained |
| 8 | resize | — | x, y = new content size (physical pixels) |
| 9 | close-requested | — | the user closed the window |
| 10 | pointer-leave | `mouse` | |

Window events (7, 8, 9) are always delivered: they tell the node about its window and trigger
nothing. Input the node's contract does not grant is refused by the host and never written.

## Algorithms

**Present (node).**

1. If not owned, try the writer lock (at most every 50 ms). On failure return *standby*.
2. Read `published` and `reading`. Pick the first buffer index in 0..3 that is neither.
3. Write the pixels and the record, CRC last.
4. Store `published = (seq << 2) | index` (release order where the language offers it).

**Take latest (host).**

1. Read `published`. Stop if it is 0 or not newer than the last frame taken.
2. Store `reading = index + 1` and re-read `published`. If it changed, start again with the newer
   frame.
3. Copy the record and the pixels out, then clear `reading`.
4. Accept only if the CRC verifies and the record's `seq` matches. Otherwise count a rejected read
   and try again on the next poll.

**Input.** The host writes event `head + 1` into slot `head % 256`, then `head`, but only while
`head - tail < 256`; otherwise it counts a drop. The node reads slot `tail % 256` while its CRC
verifies and its `seq` is `tail + 1` (a larger `seq` means events were lost), then stores `tail`.

## Rendezvous

The engine allocates surfaces per **pipeline node**, so successor generations reuse them and the
window stays open. They live under a per-user directory: `MVF_DISPLAY_DIR`, else
`$XDG_RUNTIME_DIR/mvf-display` on Linux, `$TMPDIR/mvf-display` on macOS,
`%LOCALAPPDATA%\mvf\display` on Windows. The directory is mode 0700; an engine refuses one that
another user owns or others may write.

```text
<root>/run-<pid>-<n>/owner               engine pid (a run with a dead owner is swept)
<root>/run-<pid>-<n>/<node>.<k>.surface  (+ .writer.lock, .host.lock)
<root>/run-<pid>-<n>/<node>.<k>.json     {"version":1,"node","index","title","width","height",
                                          "surface","grants":{"keyboard","mouse"},"enginePid"}
```

A node receives its surface paths in `MVF_DISPLAY_SURFACES`, joined with the platform path
separator. The descriptor is written by the engine, never by the node. The host takes the window
title, initial size and input grants only from it.
