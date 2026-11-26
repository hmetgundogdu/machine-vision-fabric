"""A processor that points at a slot it does not own (offset 0)."""
import json
import sys

sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.bad-offset",
                             "capability": "processor"}) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "shutdown":
        break
    if msg.get("type") == "execute":
        sys.stdout.write(json.dumps({"type": "result", "id": msg["id"],
                                     "frame": {"shm": {"offset": 0}}}) + "\n")
        sys.stdout.flush()
