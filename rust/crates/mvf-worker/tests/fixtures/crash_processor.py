"""A processor that dies (exit code 9) on its first frame."""
import os
from mvf_sdk import run_processor


def transform(payload, meta):
    os._exit(9)


run_processor("test.crash-processor", transform)
