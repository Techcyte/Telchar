#!/usr/bin/env python3
"""Tests release version preparation contracts."""

from __future__ import annotations

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "prepare_release", ROOT / "scripts" / "prepare-release.py"
)
assert SPEC is not None
assert SPEC.loader is not None
PREPARE_RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE_RELEASE)
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
        package = next(
            p for p in lockfile["package"] if p["name"] == "telchar-telemetry"
        )
        self.assertEqual(package["version"], RELEASE_VERSION)

    def test_release_preserves_runtime_check(self) -> None:
        root = self.prepare_copy()
        runtime_check = root / "nix/checks/nixos/oci.nix"
        before = runtime_check.read_text()

        PREPARE_RELEASE.prepare_release(root, RELEASE_VERSION)

        after = runtime_check.read_text()
        self.assertEqual(after, before)

    def test_release_updates_every_versioned_lockfile(self) -> None:
        root = self.prepare_copy()

        PREPARE_RELEASE.prepare_release(root, RELEASE_VERSION)

        for relative_path, package_names in PREPARE_RELEASE.LOCKFILE_PACKAGES:
            lockfile = tomllib.loads((root / relative_path).read_text())
            for name in package_names:
                package = next(p for p in lockfile["package"] if p["name"] == name)
                self.assertEqual(package["version"], RELEASE_VERSION)


if __name__ == "__main__":
    unittest.main()
