"""Sample module with a live-tunable config: adds a configurable offset to every byte.

Shows the other half of a `bindings` declaration. The node's config arrives here once before the first
frame and again every time an operator turns the tunable in the CLI — the process is never restarted, so
a loaded model or an open device survives the edit. Declaring `on_configure` is what makes the SDK
advertise the `configure` feature in its handshake; without it the engine falls back to closing and
reopening the node, which is what every module got before.

Raising from `configure` is a refusal: it comes back to the engine as an `error`, and the engine then
re-opens the node rather than leaving it running with a config nobody thinks it has.
"""
from mvf_sdk import blob, log, run_processor

_config = {"offset": 0}


def configure(config):
    offset = config.get("offset", 0)
    if isinstance(offset, bool) or not isinstance(offset, (int, float)):
        raise ValueError("offset must be a number, got {!r}".format(offset))
    _config["offset"] = int(offset)
    log("offset is now {}".format(_config["offset"]))


def transform(payload, meta):
    offset = _config["offset"]
    return blob(bytes((b + offset) % 256 for b in payload.memory))


run_processor("py.tunable-transformer", transform, on_configure=configure)
