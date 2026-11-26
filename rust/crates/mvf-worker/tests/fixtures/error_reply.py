"""Raises on odd sequences: the SDK replies `error` and keeps serving (managed failure)."""
from mvf_sdk import run_classifier


def classify(payload, meta):
    if meta.get("sequence", 0) % 2 == 1:
        raise ValueError("odd frames are rejected")
    return ("even", None, None, None)


run_classifier("test.error-reply", classify)
