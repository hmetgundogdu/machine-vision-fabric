"""Reports its own pid in every classification, proving one process serves every cycle."""
import os
from mvf_sdk import run_classifier, log


def classify(payload, meta):
    log("frame {} ({} bytes)".format(meta.get("sequence"), len(payload.memory)))
    return ("pid", float(os.getpid()), "pid", str(meta.get("sequence")))


run_classifier("test.pid-classifier", classify)
