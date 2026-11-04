"""Retained input capacity admission uses actual allocated files and free space."""
import importlib.util
import os
from pathlib import Path
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("retained_store", ROOT / "deploy/microvm/retained_store.py")
STORE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STORE)


class StoreCapacityTests(unittest.TestCase):
    def test_retained_capacity_rejects_full_store_and_preserves_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = root / "nix/store"
            inputs.mkdir(parents=True)
            data = inputs / "input"
            data.write_bytes(b"x" * 8192)
            os.symlink("/", inputs / "external")
            STORE.check_capacity(root, 16384, 0, time.monotonic() + 5)
            with self.assertRaisesRegex(OSError, "retained store capacity"):
                STORE.check_capacity(root, 1, 0, time.monotonic() + 5)
            with self.assertRaisesRegex(OSError, "retained store capacity"):
                STORE.check_capacity(root, 16384, 2**63, time.monotonic() + 5)
            self.assertEqual(data.read_bytes(), b"x" * 8192)

    def test_capacity_scan_obeys_deadline_and_rejects_invalid_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "nix/store").mkdir(parents=True)
            with self.assertRaises(TimeoutError):
                STORE.check_capacity(directory, 1024, 0, time.monotonic() - 1)
            with self.assertRaises(ValueError):
                STORE.check_capacity(directory, 0, 0, time.monotonic() + 5)
