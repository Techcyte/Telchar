#!/usr/bin/env python3
"""Exercise private worker-store population with unrelated daemon state present."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile


script = Path(__file__).with_name("prepare-worker-store.sh")
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    store = root / "store"
    store.mkdir()
    package = store / "package"
    package.mkdir()
    executable = package / "tool"
    executable.write_text(f"#!{shutil.which('bash')}\nprintf 'worker-tool-ran\\n'\n")
    executable.chmod(0o555)
    (package / "alias").symlink_to("tool")
    data = store / "certificate"
    data.write_bytes(b"public trust fixture\n")
    data.chmod(0o444)
    lock = store / "package.lock"
    lock.touch(mode=0o000)
    paths = root / "store-paths"
    paths.write_text(f"{package}\n{data}\n")
    destination = root / "worker-store"
    destination.mkdir()
    try:
        subprocess.run(["bash", str(script), str(paths), str(destination)], check=True)
        assert not (destination / lock.name).exists()
        assert (destination / data.name).read_bytes() == data.read_bytes()
        assert (destination / package.name / "alias").is_symlink()
        assert os.readlink(destination / package.name / "alias") == "tool"
        result = subprocess.run(
            [str(destination / package.name / "alias")],
            check=True,
            capture_output=True,
            text=True,
        )
        assert result.stdout == "worker-tool-ran\n", result.stdout
        paths.write_text(f"{store / 'missing-required-input'}\n")
        result = subprocess.run(
            ["bash", str(script), str(paths), str(destination)],
            capture_output=True,
            text=True,
        )
        assert result.returncode != 0
        assert "missing-required-input" in result.stderr, result.stderr
        assert "No such file or directory" in result.stderr, result.stderr
    finally:
        lock.chmod(0o600)
print("WORKER_STORE_POPULATION_PASS")
