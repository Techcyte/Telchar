"""Ancestor process identities for nested execution containment contracts."""
import contextlib
import subprocess


@contextlib.contextmanager
def ancestor_proc(root):
    path = root / "observer-proc"
    path.mkdir()
    subprocess.run(["mount", "--bind", "/proc", str(path)], check=True, capture_output=True)
    try:
        yield path
    finally:
        subprocess.run(["umount", str(path)], check=True, capture_output=True)
