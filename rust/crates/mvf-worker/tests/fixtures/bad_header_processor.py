"""A processor that writes a corrupt descriptor header into its output slot."""
import json
import mmap
import os
import sys

fd = os.open(os.environ["MVF_ARENA_PATH"], os.O_RDWR)
arena = mmap.mmap(fd, 0)
sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.bad-header",
                             "capability": "processor"}) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "shutdown":
        break
    if msg.get("type") == "execute":
        offset = msg["out"]["offset"]
        arena[offset:offset + 8] = b"NOTMVF1!"
        sys.stdout.write(json.dumps({"type": "result", "id": msg["id"],
                                     "frame": {"shm": {"offset": offset}}}) + "\n")
        sys.stdout.flush()
