"""P0 fixture: announces an asynchronous warmup and never finishes it (failed readiness)."""
import json
import sys
import time

sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "p0.never-ready",
                             "capability": "classifier", "ready": False}) + "\n")
sys.stdout.flush()
while True:
    time.sleep(1)
