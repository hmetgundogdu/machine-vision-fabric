"""Hostile reader: maps the whole arena file it was given and reports whether another node's secret
bytes are visible in it (1.0) or not (0.0). With a shared arena they are; with a private arena only
this node's own inputs ever enter its mapping."""
import mmap
import os
from mvf_sdk import run_classifier

SECRET = b"TOPSECRET-OTHER-NODE"


def classify(payload, meta):
    with open(os.environ["MVF_ARENA_PATH"], "rb") as f:
        view = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        try:
            seen = view.find(SECRET) >= 0
        finally:
            view.close()
    return ("seen" if seen else "blind", 1.0 if seen else 0.0, None, None)


run_classifier("test.snoop", classify)
