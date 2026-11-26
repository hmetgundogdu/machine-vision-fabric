"""Handshakes, then never reads stdin again: it does not exit on stdin EOF, so only the OS can stop
it when the host dies (PR_SET_PDEATHSIG on Linux, a kill-on-close Job Object on Windows)."""
import json
import sys
import time

sys.stdout.write(json.dumps({"type": "hello", "protocol": 1, "moduleId": "test.deaf",
                             "capability": "classifier"}) + "\n")
sys.stdout.flush()
time.sleep(600)
