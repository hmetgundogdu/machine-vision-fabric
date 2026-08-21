from mvf_sdk import analysis, blob, run_analyzer


def analyze(payload, meta):
    frame = bytes(payload.memory)
    mean = sum(frame) / len(frame) if frame else 0.0
    label = "black" if mean < 10 else "ok"

    return analysis(
        frame=blob(frame),
        label=label,
        measurement=mean,
        unit="mean-byte",
        value={
            "cameraId": meta.get("cameraId"),
            "sequence": meta.get("sequence"),
            "mean": mean,
            "label": label,
        },
    )


run_analyzer("py.brightness-analyzer", analyze)
