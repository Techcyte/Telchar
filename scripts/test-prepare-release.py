#!/usr/bin/env python3
"""Tests release version preparation contracts."""

from __future__ import annotations

import importlib.util
import shutil
import tempfile
import tomllib
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
CURRENT_VERSION = PREPARE_RELEASE.current_version(ROOT)
RELEASE_VERSION = "2026.8.2"


class PrepareReleaseTest(unittest.TestCase):
    def prepare_copy(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        for relative_path in (
            *PREPARE_RELEASE.PACKAGE_MANIFESTS,
            *(path for path, _ in PREPARE_RELEASE.LOCKFILE_PACKAGES),
            Path("nix/packages.nix"),
            Path("nix/checks/nixos/oci.nix"),
            Path("tests/ssh_ingress_test.sh"),
        ):
            destination = root / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative_path, destination)
        return root

    def test_release_versions_shared_telemetry(self) -> None:
        root = self.prepare_copy()
        relative = Path("crates/telchar-telemetry/Cargo.toml")
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, destination)
        PREPARE_RELEASE.prepare_release(root, RELEASE_VERSION)
        manifest = tomllib.loads(destination.read_text())
        self.assertEqual(manifest["package"]["version"], RELEASE_VERSION)
        lockfile = tomllib.loads((root / "Cargo.lock").read_text())
        package = next(p for p in lockfile["package"] if p["name"] == "telchar-telemetry")
        self.assertEqual(package["version"], RELEASE_VERSION)

    def test_oci_runtime_references_follow_image_metadata(self) -> None:
        root = self.prepare_copy()
        runtime_check = root / "nix/checks/nixos/oci.nix"
        before = runtime_check.read_text()

        PREPARE_RELEASE.prepare_release(root, RELEASE_VERSION)

        after = runtime_check.read_text()
        self.assertEqual(after, before)
        self.assertNotIn(f"telchar:{CURRENT_VERSION}", after)
        self.assertNotIn(f"telchar-nomad-worker:{CURRENT_VERSION}", after)
        self.assertIn("${telcharImage.imageName}:${telcharImage.imageTag}", after)
        self.assertIn(
            "${nomadWorkerImage.imageName}:${nomadWorkerImage.imageTag}", after
        )

    def test_release_updates_every_versioned_lockfile(self) -> None:
        root = self.prepare_copy()

        PREPARE_RELEASE.prepare_release(root, RELEASE_VERSION)

        for relative_path, _ in PREPARE_RELEASE.LOCKFILE_PACKAGES:
            lockfile = (root / relative_path).read_text()
            self.assertNotIn(f'version = "{CURRENT_VERSION}"', lockfile)
            self.assertIn(f'version = "{RELEASE_VERSION}"', lockfile)

    def test_oci_metadata_uses_package_version(self) -> None:
        contract = (ROOT / "nix/tests/oci-images.nix").read_text()

        self.assertNotIn(f'imageTag == "{CURRENT_VERSION}"', contract)
        self.assertEqual(contract.count("imageTag == telchar.version"), 4)

    def test_ssh_ingress_test_uses_loaded_archive_tag(self) -> None:
        script = (ROOT / "tests/ssh_ingress_test.sh").read_text()

        self.assertNotIn(f"telchar-ssh-ingress:{CURRENT_VERSION}", script)
        self.assertIn("docker load", script)
        self.assertIn("Loaded image:", script)


if __name__ == "__main__":
    unittest.main()
