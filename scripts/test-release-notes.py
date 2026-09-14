#!/usr/bin/env python3

import subprocess
import tempfile
import unittest
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
RELEASE_WORKFLOW = REPOSITORY_ROOT / ".github" / "workflows" / "release.yml"


class ReleaseNotesTests(unittest.TestCase):
    def test_release_workflow_passes_multiline_image_notes(self) -> None:
        workflow = RELEASE_WORKFLOW.read_text()
        script = workflow.split("      - name: Create draft GitHub release\n", 1)[1]
        script = script.split("\n      - name: Publish release images\n", 1)[0]
        shell = script.split("        run: |\n", 1)[1]
        shell = "\n".join(line[10:] for line in shell.splitlines())

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            arguments = root / "arguments"
            gh = root / "gh"
            gh.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\0' \"$@\" > \"$CAPTURED_ARGUMENTS\"\n"
            )
            gh.chmod(0o755)
            subprocess.run(
                ["bash", "-eu", "-o", "pipefail", "-c", shell],
                check=True,
                env={
                    "CAPTURED_ARGUMENTS": str(arguments),
                    "GITHUB_SHA": "0123456789abcdef",
                    "PATH": f"{root}:/usr/bin:/bin",
                    "RELEASE_VERSION": "2026.9.1",
                },
            )

            values = arguments.read_bytes().split(b"\0")
            notes = values[values.index(b"--notes") + 1].decode()
            self.assertEqual(
                notes,
                "OCI images:\n"
                "- ghcr.io/techcyte/telchar:2026.9.1\n"
                "- ghcr.io/techcyte/telchar-nomad-worker:2026.9.1\n"
                "- ghcr.io/techcyte/telchar-nix-daemon:2026.9.1\n"
                "- ghcr.io/techcyte/telchar-ssh-ingress:2026.9.1",
            )


if __name__ == "__main__":
    unittest.main()
