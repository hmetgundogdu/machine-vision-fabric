"""MVF Storage Fabric helpers for Python nodes (#31). Standard library only.

A node declares logical mounts in its contract (``spec.storage.mounts``); the deployment binds them
to volumes. The node reads and writes them with ordinary file APIs (``open``, ``pathlib``, OpenCV).
MVF only tells it where they are: ``MVF_VOLUME_<NAME>`` holds the path, which is a host directory
for a worker and the mount target (``/models``) inside an OCI sandbox.
"""
import os
import pathlib

__all__ = ["volume", "write_atomic"]


def _env(name):
    return "MVF_VOLUME_" + name.upper().replace("-", "_")


def volume(name, required=True):
    """The path of logical mount ``name`` (a ``pathlib.Path``), or None for an unbound optional one."""
    value = os.environ.get(_env(name))
    if value is None:
        if required:
            raise KeyError("mount `{}` is not bound ({} is unset)".format(name, _env(name)))
        return None
    return pathlib.Path(value)


def write_atomic(path, data):
    """Publish ``data`` at ``path`` atomically: readers see the old file or the whole new one,
    never a partial write. Writes a temporary file in the same directory, flushes it to disk and
    renames it over ``path`` (``os.replace``, atomic on POSIX and on NTFS)."""
    path = pathlib.Path(path)
    tmp = path.with_name(".{}.{}.tmp".format(path.name, os.getpid()))
    with open(tmp, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    return path
