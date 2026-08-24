#!/usr/bin/env python3
"""Tests release version preparation contracts."""

from __future__ import annotations

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "prepare_release", ROOT / "scripts" / "prepare-release.py"
)
assert SPEC is not None
assert SPEC.loader is not None
PREPARE_RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE_RELEASE)


class PrepareReleaseTest(unittest.TestCase):
    def test_oci_runtime_references_follow_image_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative_path in (
                *PREPARE_RELEASE.PACKAGE_MANIFESTS,
                Path("Cargo.lock"),
                Path("nix/packages.nix"),
                Path("nix/tests/oci-images.nix"),
                Path("nix/checks/nixos/oci.nix"),
            ):
                destination = root / relative_path
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative_path, destination)

            runtime_check = root / "nix/checks/nixos/oci.nix"
            before = runtime_check.read_text()
            PREPARE_RELEASE.prepare_release(root, "2026.8.1")
            after = runtime_check.read_text()

            self.assertEqual(after, before)
            self.assertNotIn("telchar:2026.8.0", after)
            self.assertNotIn("telchar-nomad-worker:2026.8.0", after)
            self.assertIn("${telcharImage.imageName}:${telcharImage.imageTag}", after)
            self.assertIn(
                "${nomadWorkerImage.imageName}:${nomadWorkerImage.imageTag}", after
            )


if __name__ == "__main__":
    unittest.main()
