"""Takes its `label` config live (`on_configure`) and reports its pid, so a test can tell a live
reconfigure (same process) from a reopen (new process). Refuses the label `refuse` when it arrives
live (it accepts anything at startup), so the engine has to reopen it to apply that value."""
import os
from mvf_sdk import run_classifier

_config = {}
_calls = [0]


def classify(payload, meta):
    return (_config.get("label", "none"), float(os.getpid()), "pid", None)


def on_configure(config):
    _calls[0] += 1
    if _calls[0] > 1 and config.get("label") == "refuse":
        raise ValueError("this module cannot use label 'refuse'")
    _config.clear()
    _config.update(config)


run_classifier("test.configurable", classify, on_configure=on_configure)
