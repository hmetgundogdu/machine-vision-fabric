"""Announces an asynchronous warmup and never finishes it."""
import json
import sys
import time

sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.never-ready",
                             "capability": "classifier", "ready": False}) + "\n")
sys.stdout.flush()
while True:
    time.sleep(1)
