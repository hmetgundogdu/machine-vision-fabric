"""Sample polyglot transformer: inverts every byte of a frame (b -> 255 - b).

Demonstrates a Python module that produces a NEW frame in the middle of a .NET graph. The input
arrives as a zero-copy Payload over shared memory and the output is written straight back into the
arena (no base64, no bytes on the pipe). PYTHONPATH points at src/sdk/python so `mvf_sdk` imports.
"""
from mvf_sdk import run_processor, blob, log


def transform(payload, meta):
    inverted = bytes(255 - b for b in payload.memory)
    log("inverted {} bytes".format(len(inverted)))
    return blob(inverted)


run_processor("py.invert-transformer", transform)
