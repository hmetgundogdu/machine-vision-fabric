"""Warms up for 400 ms (a model load stand-in), then crashes once, on the frame with sequence 5.
A marker file per host process and camera id makes only the first worker crash, so a restart (warm
or cold) carries on."""
import os
import tempfile
import time
from mvf_sdk import run_classifier



def marker(meta):
    return os.path.join(tempfile.gettempdir(), "mvf-slow-crash-{}-{}".format(os.getppid(), meta.get("cameraId")))


def on_start():
    time.sleep(0.4)


def classify(payload, meta):
    if meta.get("sequence") == 5 and not os.path.exists(marker(meta)):
        open(marker(meta), "w").close()
        os._exit(3)
    return ("ok", float(os.getpid()), "pid", str(meta.get("sequence")))


run_classifier("test.slow-crash", classify, on_start=on_start)
