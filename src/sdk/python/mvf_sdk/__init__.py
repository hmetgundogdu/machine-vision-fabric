"""Minimal MVF Python module SDK (control plane over stdio, newline-delimited JSON).

A classifier module is just a function ``classify(data: bytes, frame: dict) -> tuple``
returning ``(label, measurement, unit, details)``. The SDK owns the stdio message loop.
See ../../../protocol/README.md for the wire contract. Local only — no network.
"""
import sys
import json
import base64

__all__ = ["run_classifier"]


def _send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def run_classifier(module_id, classify):
    """Run the stdio loop for a classifier module.

    ``classify(data, frame)`` must return ``(label, measurement, unit, details)``;
    ``measurement``/``unit``/``details`` may be ``None``.
    """
    _send({"type": "hello", "protocol": 1, "moduleId": module_id, "capability": "classifier"})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        msg_type = msg.get("type")
        if msg_type == "shutdown":
            break
        if msg_type != "execute":
            continue
        try:
            frame = msg.get("frame") or {}
            data = base64.b64decode(frame.get("dataBase64", "")) if frame.get("dataBase64") else b""
            label, measurement, unit, details = classify(data, frame)
            _send({
                "type": "result",
                "id": msg.get("id"),
                "classification": {
                    "label": label,
                    "measurement": measurement,
                    "unit": unit,
                    "details": details,
                },
            })
        except Exception as exc:  # report per-request failure, keep serving
            _send({"type": "error", "id": msg.get("id"), "message": str(exc)})
