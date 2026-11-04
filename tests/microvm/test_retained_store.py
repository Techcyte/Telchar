"""Real Nix overlay jobs over one retained local input generation.

Run in a disposable root-mapped mount namespace; sandboxing stays enabled.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]


class RetainedStoreTests(unittest.TestCase):
    def test_sequential_jobs_reuse_inputs_without_retaining_private_outputs(self):
        spec = importlib.util.spec_from_file_location(
            "retained_store", ROOT / "deploy/microvm/retained_store.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        nix = shutil.which("nix")
        shell = Path(shutil.which("bash")).resolve()
        package = shell.parent.parent
        environment = os.environ.copy()
        environment.pop("NIX_REMOTE", None)
        environment["NIX_CONFIG"] = (
            "experimental-features = nix-command local-overlay-store read-only-local-store\n"
            "builders =\nbuild-users-group =\nsandbox = true\nsubstituters =\n")

        def run(*arguments):
            result = subprocess.run([nix, *map(str, arguments)], env=environment,
                                    capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("warning:", result.stderr)
            return result.stdout.strip()

        with tempfile.TemporaryDirectory(prefix="telchar-retained-store-") as directory:
            root = Path(directory)
            lower = root / "inputs"
            run("copy", "--to", f"local?root={lower}", "--no-check-sigs", package)
            database = lower / "nix/var/nix/db/db.sqlite"
            before = hashlib.sha256(database.read_bytes()).digest()
            marker = root / "host-only"
            marker.touch()
            outputs = []
            for index in range(2):
                job = root / f"job-{index}"
                with module.job_daemon(lower, job, nix, environment,
                                       time.monotonic() + 30) as uri:
                    self.assertTrue(uri.startswith("unix://"))
                    self.assertTrue((job / "daemon.sock").is_socket())
                    self.assertEqual(run("path-info", "--store", uri, package), str(package))
                    script = f'test ! -e {marker}; printf job-{index} > "$out"'
                    expression = (
                        f'derivation {{ name = "retained-job-{index}"; '
                        f'system = builtins.currentSystem; builder = "{shell}"; '
                        f'shell = builtins.storePath "{package}"; '
                        f'args = [ "-euc" {json.dumps(script)} ]; }}')
                    output = Path(run("build", "--store", uri, "--impure", "--expr",
                                      expression, "--no-link", "--print-out-paths"))
                    self.assertEqual((job / "upper" / output.name).read_text(), f"job-{index}")
                    self.assertFalse((job / "upper" / package.name).exists())
                    for previous in outputs:
                        result = subprocess.run([nix, "path-info", "--store", uri, previous],
                                                env=environment, capture_output=True, text=True)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("is not valid", result.stderr)
                    outputs.append(str(output))
                self.assertFalse(job.exists(), "private job state survives teardown")
                self.assertFalse((lower / "nix/store" / output.name).exists())
            self.assertEqual(hashlib.sha256(database.read_bytes()).digest(), before)
            failed_job = root / "failed-job"
            with self.assertRaisesRegex(RuntimeError, "cancelled job"):
                with module.job_daemon(lower, failed_job, nix, environment,
                                       time.monotonic() + 30) as uri:
                    self.assertEqual(run("path-info", "--store", uri, package), str(package))
                    raise RuntimeError("cancelled job")
            self.assertFalse(failed_job.exists())
            self.assertEqual(hashlib.sha256(database.read_bytes()).digest(), before)
            expired_job = root / "expired-job"
            with self.assertRaisesRegex(TimeoutError, "readiness expired"):
                with module.job_daemon(lower, expired_job, nix, environment,
                                       time.monotonic() - 1):
                    self.fail("expired job reached execution")
            self.assertFalse(expired_job.exists())


if __name__ == "__main__":
    unittest.main()
