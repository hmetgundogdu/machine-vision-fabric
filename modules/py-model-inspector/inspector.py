"""Model inspector (Storage Fabric, #31): business logic with ordinary file I/O, no MVF file API.

At readiness it loads the model from the `model` mount (the first `*.onnx`), checks its SHA-256 and
reads the ONNX header (IR version, producer, opset) with a minimal protobuf reader: inference
itself is the node's business. Every frame yields one record published atomically into `results`
(temporary file + fsync + rename), and a work file in `tmp` scratch space when it is mounted.
"""
import errno
import hashlib
import json
import os
import time

from mvf_sdk import analysis, log, run_analyzer
from mvf_sdk.storage import volume, write_atomic

_state = {"model": None, "scratchBytes": 64 * 1024}
# Names this process's records. Not the pid: inside a container every process is pid 1, so a
# restarted or re-run node would overwrite the records it published before.
_RUN = os.urandom(4).hex()


def _varint(b, i):
    shift = n = 0
    while True:
        c = b[i]
        i += 1
        n |= (c & 0x7F) << shift
        shift += 7
        if c < 0x80:
            return n, i


def onnx_info(data):
    """IR version, producer and opset of an ONNX ModelProto (fields 1, 2 and 8)."""
    info, i = {}, 0
    while i < len(data):
        key, i = _varint(data, i)
        field, wire = key >> 3, key & 7
        if wire == 0:
            value, i = _varint(data, i)
            if field == 1:
                info["irVersion"] = value
        elif wire == 2:
            n, i = _varint(data, i)
            chunk, i = data[i:i + n], i + n
            if field == 2:
                info["producer"] = chunk.decode("utf-8", "replace")
            elif field == 8:
                j = 0
                while j < len(chunk):
                    k, j = _varint(chunk, j)
                    if k & 7 == 0:
                        v, j = _varint(chunk, j)
                        if k >> 3 == 2:
                            info["opset"] = v
                    else:
                        n2, j = _varint(chunk, j)
                        j += n2
        elif wire == 1:
            i += 8
        elif wire == 5:
            i += 4
        else:
            raise ValueError("not an ONNX model (wire type {})".format(wire))
    if "irVersion" not in info:
        raise ValueError("not an ONNX model (no ir_version)")
    return info


def on_start():
    t0 = time.perf_counter()
    path = sorted(volume("model").glob("*.onnx"))[0]
    data = path.read_bytes()
    model = {"file": path.name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    model.update(onnx_info(data))
    model["loadMs"] = round((time.perf_counter() - t0) * 1000, 3)
    _state["model"] = model
    log("model {file} sha256={short} ir={ir} producer={producer} opset={opset} ({bytes} bytes, {ms} ms)".format(
        file=model["file"], short=model["sha256"][:12], ir=model.get("irVersion"),
        producer=model.get("producer"), opset=model.get("opset"), bytes=model["bytes"],
        ms=model["loadMs"]))


def on_configure(config):
    if "scratchBytes" in config:
        _state["scratchBytes"] = int(config["scratchBytes"])


def analyze(payload, meta):
    seq = meta.get("sequence")
    model = _state["model"]
    sample = bytes(payload.memory[:4096])
    record = {"sequence": seq, "run": _RUN, "model": model["sha256"], "producer": model.get("producer"),
              "mean": round(sum(sample) / max(1, len(sample)), 3)}
    write_atomic(volume("results") / "seq-{}-{:06d}.json".format(_RUN, seq),
                 json.dumps(record).encode())

    scratch, scratch_error = volume("tmp", required=False), None
    if scratch is not None:
        try:
            with open(scratch / "work.bin", "wb") as f:
                f.write(b"\0" * _state["scratchBytes"])
        except OSError as e:
            scratch_error = errno.errorcode.get(e.errno, str(e.errno))
    try:
        (volume("model") / "probe.txt").write_bytes(b"x")
        model_writable = True
    except OSError:
        model_writable = False
    return analysis(value={
        "sequence": seq,
        "model": model,
        "scratch": None if scratch is None else (scratch_error or "ok"),
        "modelWritable": model_writable,
        "results": str(volume("results")),
    })


run_analyzer("py.model-inspector", analyze, on_start=on_start, on_configure=on_configure)
