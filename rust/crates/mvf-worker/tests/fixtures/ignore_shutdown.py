"""Answers execute but ignores `shutdown` and stdin EOF: only a forced kill stops it."""
import json
import sys
import time

sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.stubborn",
                             "capability": "classifier"}) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "execute":
        sys.stdout.write(json.dumps({"type": "result", "id": msg["id"],
                                     "classification": {"label": "stubborn"}}) + "\n")
        sys.stdout.flush()
while True:
    time.sleep(1)
