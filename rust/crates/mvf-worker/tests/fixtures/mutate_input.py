"""Writes into its read-only input payload (forbidden by the protocol) and replies normally."""
import json
import mmap
import os
import sys

fd = os.open(os.environ["MVF_ARENA_PATH"], os.O_RDWR)
arena = mmap.mmap(fd, 0)
sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.mutate-input",
                             "capability": "classifier"}) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "shutdown":
        break
    if msg.get("type") == "execute":
        offset = msg["frame"]["shm"]["offset"] + 192
        arena[offset] = (arena[offset] + 1) % 256
        sys.stdout.write(json.dumps({"type": "result", "id": msg["id"],
                                     "classification": {"label": "mutated"}}) + "\n")
        sys.stdout.flush()
