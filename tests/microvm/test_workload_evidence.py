"""Exercise archive size and digest validation through actual files."""
import base64
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("evidence", Path(__file__).with_name("verify-workload-evidence.py"))
assert spec is not None and spec.loader is not None
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


class Archives(unittest.TestCase):
    def test_metadata_and_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "output.nar"
            path.write_bytes(b"preserved archive bytes")
            metadata = {"narSize": path.stat().st_size, "narHash": "sha256-" + base64.b64encode(hashlib.sha256(path.read_bytes()).digest()).decode()}
            evidence.verify_archive(path, metadata)
            path.write_bytes(b"corrupted archive bytes")
            with self.assertRaises(AssertionError):
                evidence.verify_archive(path, metadata)
            path.write_bytes(b"truncated")
            with self.assertRaises(AssertionError):
                evidence.verify_archive(path, metadata)


if __name__ == "__main__":
    unittest.main()
