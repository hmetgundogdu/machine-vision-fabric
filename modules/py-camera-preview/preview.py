"""Camera preview (Display Fabric, #30): business logic only, no UI toolkit.

Every frame goes to window 1 (live view). A click in it sets the region of interest, marked with a
crosshair; window 2 shows that region at a quarter of the frame size, which the display host scales
up to its window: a zoomed detail view. The JSON output reports what was presented and the region,
so a click becomes typed data in the graph, never a machine action.
"""
from mvf_sdk import analysis, run_analyzer
from mvf_sdk import display

_FORMATS = {1: "gray8", 3: "rgb8", 4: "rgba8"}
_state = {"roi": None, "clicks": 0}


def _crosshair(buf, width, height, channels, x, y, arm=7):
    """Invert a small crosshair at (x, y) in place."""
    def flip(px, py):
        if 0 <= px < width and 0 <= py < height:
            off = (py * width + px) * channels
            for k in range(channels):
                buf[off + k] = 255 - buf[off + k]
    for d in range(-arm, arm + 1):
        flip(x + d, y)
        if d:
            flip(x, y + d)


def analyze(payload, meta):
    shape = tuple(payload.shape)
    if len(shape) == 2:
        shape = shape + (1,)
    if len(shape) != 3 or shape[2] not in _FORMATS:
        return analysis(value={"presented": False, "reason": "not an 8-bit image"})
    height, width, channels = shape
    fmt = _FORMATS[channels]
    windows = display.surfaces()
    if not windows:
        return analysis(value={"presented": False, "reason": "headless"})

    live = windows[0]
    for event in live.poll_input():
        if event.kind == "pointer-down" and event.code == 0:
            _state["roi"] = (int(event.x), int(event.y))
            _state["clicks"] += 1
    roi = _state["roi"]
    if roi is None:
        shown = live.present(payload.memory, width, height, fmt)
    else:
        marked = bytearray(payload.memory)
        _crosshair(marked, width, height, channels, min(roi[0], width - 1), min(roi[1], height - 1))
        shown = live.present(marked, width, height, fmt)

    if len(windows) > 1:
        cx, cy = roi if roi is not None else (width // 2, height // 2)
        cw, ch = max(1, width // 4), max(1, height // 4)
        x0 = min(max(0, cx - cw // 2), width - cw)
        y0 = min(max(0, cy - ch // 2), height - ch)
        row = width * channels
        mem = payload.memory
        crop = b"".join(mem[(y0 + y) * row + x0 * channels:(y0 + y) * row + (x0 + cw) * channels]
                        for y in range(ch))
        windows[1].present(crop, cw, ch, fmt)

    host = live.host()
    return analysis(value={
        "presented": shown,
        "sequence": meta.get("sequence"),
        "windows": len(windows),
        "hostAttached": bool(host and host.attached()),
        "closed": bool(host and host.closed),
        "requested": [host.requested_width, host.requested_height] if host else None,
        "roi": list(roi) if roi is not None else None,
        "clicks": _state["clicks"],
        "lostEvents": live.lost_events,
    })


run_analyzer("py.camera-preview", analyze)
