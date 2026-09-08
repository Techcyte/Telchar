#!/usr/bin/env python3
"""Tests OCI image publication input contracts."""

from __future__ import annotations

import os
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PUBLISH = ROOT / "scripts" / "publish-oci-images.sh"


class PublishOciImagesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.release_version = subprocess.run(
            ["nix", "eval", "--raw", ".#packages.x86_64-linux.telchar.version"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=True,
        ).stdout

    def run_publisher(self, image_tag: str) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["IMAGE_TAG"] = image_tag
        environment.pop("RELEASE_VERSION", None)
        environment.pop("GITHUB_ACTOR", None)
        environment.pop("GHCR_TOKEN", None)
        return subprocess.run(
            [PUBLISH],
            cwd=ROOT,
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_main_tag_is_accepted(self) -> None:
        result = self.run_publisher("main")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("GITHUB_ACTOR is required", result.stderr)

    def test_release_tag_is_accepted(self) -> None:
        result = self.run_publisher(self.release_version)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("GITHUB_ACTOR is required", result.stderr)

    def test_other_moving_tag_is_rejected(self) -> None:
        result = self.run_publisher("latest")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("image tag must be main or match YYYY.M.PATCH", result.stderr)


if __name__ == "__main__":
    unittest.main()
