"""Speaks a protocol version this host does not support."""
import json
import sys
import time

sys.stdout.write(json.dumps({"type": "hello", "protocol": 3, "moduleId": "test.v2",
                             "capability": "classifier"}) + "\n")
sys.stdout.flush()
time.sleep(30)
