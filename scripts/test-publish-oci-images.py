#!/usr/bin/env python3
"""Tests OCI image publication input contracts."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
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

    def test_images_are_published_with_selected_tag(self) -> None:
        cases = [(system, tag) for system in ("x86_64-linux", "aarch64-linux")
                 for tag in ("main", self.release_version)]
        for system, tag in cases:
            images = (["telchar-microvm-execution"] if system == "aarch64-linux" else
                      ["telchar", "telchar-nomad-worker", "telchar-nix-daemon", "telchar-ssh-ingress"])
            with self.subTest(system=system, tag=tag), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                calls = root / "calls"
                nix = root / "nix"
                nix.write_text(
                    "#!/usr/bin/env python3\n"
                    "import json, os, sys\n"
                    "with open(os.environ['CAPTURED_CALLS'], 'a') as output:\n"
                    "    output.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                    "if sys.argv[1] == 'eval':\n"
                    "    print(os.environ['PACKAGE_VERSION'], end='')\n"
                    "elif sys.argv[1] == 'build':\n"
                    "    print('/isolated/microvm.tar.gz')\n"
                    "elif 'login' in sys.argv:\n"
                    "    sys.stdin.read()\n"
                )
                nix.chmod(0o755)
                result = subprocess.run(
                    [PUBLISH], cwd=ROOT, text=True, capture_output=True,
                    env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                         "CAPTURED_CALLS": str(calls), "PACKAGE_VERSION": self.release_version,
                         "IMAGE_TAG": tag, "IMAGE_SYSTEM": system,
                         "GITHUB_ACTOR": "test-actor", "GHCR_TOKEN": "test-token"},
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                commands = [json.loads(line) for line in calls.read_text().splitlines()]
                self.assertEqual(
                    [command for command in commands if command[0] == "build"],
                    [["build", "--no-link", "--print-out-paths",
                      f".#packages.{system}.{image}-oci"] for image in images],
                )
                self.assertEqual(
                    [command for command in commands if "copy" in command],
                    [["develop", "--command", "skopeo", "copy",
                      "docker-archive:/isolated/microvm.tar.gz",
                      f"docker://ghcr.io/techcyte/{image}:{tag}"] for image in images],
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
