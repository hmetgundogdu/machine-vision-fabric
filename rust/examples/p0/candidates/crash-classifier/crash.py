"""P0 fixture: a detector update that crashes (exit 9) on its first frame (post-promotion failure)."""
import os
from mvf_sdk import run_classifier

run_classifier("p0.crash-classifier", lambda payload, meta: os._exit(9))
