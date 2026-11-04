"""Real Nix overlay jobs over one retained local input generation.

Run in a disposable root-mapped mount namespace; sandboxing stays enabled.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import threading
import tempfile
import time
import unittest
from unittest.mock import patch
import sys

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
                worker, runtime = socket.socketpair()
                self.addCleanup(worker.close)
                self.addCleanup(runtime.close)
                handoff = []
                def prepare():
                    worker.sendall(b"prepared\n")
                    response = bytearray()
                    while not response.endswith(b"\n"):
                        chunk = worker.recv(1)
                        if not chunk:
                            break
                        response.extend(chunk)
                    handoff.append(response.decode().strip())
                thread = threading.Thread(target=prepare)
                thread.start()
                with module.prepared_job(runtime, lower, job, nix, environment,
                                         time.monotonic() + 30) as uri:
                    thread.join(timeout=3)
                    self.assertFalse(thread.is_alive())
                    self.assertEqual(handoff, [uri])
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
            sys.path.insert(0, str(ROOT / "deploy/microvm"))
            self.addCleanup(sys.path.remove, str(ROOT / "deploy/microvm"))
            import execution
            daemon_environment = environment.copy()
            daemon_environment["NIX_DAEMON_SOCKET_PATH"] = str(root / "prepare-daemon.sock")
            daemon_environment["NIX_CONFIG"] += f"store = local?root={lower}\n"
            daemon_environment["TELCHAR_MICROVM_RETAINED_ROOT"] = str(lower)
            daemon_environment["TELCHAR_MICROVM_NIX"] = nix
            daemon_environment["TELCHAR_MICROVM_MAXIMUM_STORE_BYTES"] = str(2**40)
            daemon_environment["TELCHAR_MICROVM_MINIMUM_FREE_BYTES"] = "0"
            client = root / "worker.py"
            client.write_text(
                "import os, socket, subprocess\n"
                "channel = socket.socket(socket.AF_UNIX)\n"
                "channel.connect(os.environ['TELCHAR_MICROVM_JOB_SOCKET'])\n"
                "channel.sendall(b'prepared\\n')\n"
                "store = channel.makefile('rb').readline().decode().strip()\n"
                f"result = subprocess.run([{nix!r}, 'build', '--store', store, '--impure', "
                f"'--expr', {expression!r}, '--no-link'], capture_output=True)\n"
                "assert result.returncode == 0, result.stderr\n"
                "assert b'error:' not in result.stderr, result.stderr\n")
            with patch.dict(os.environ, daemon_environment):
                self.assertTrue(execution.supervise(
                    [nix, 'daemon'], [sys.executable, str(client)],
                    [nix, 'store', 'ping', '--store',
                     'unix://' + str(root / 'prepare-daemon.sock')], 30))
            self.assertEqual(hashlib.sha256(database.read_bytes()).digest(), before)
            denied = root / "denied-worker"
            daemon_environment["TELCHAR_MICROVM_MAXIMUM_STORE_BYTES"] = "1"
            with patch.dict(os.environ, daemon_environment):
                self.assertFalse(execution.supervise(
                    [nix, 'daemon'], [sys.executable, '-c', f'open({str(denied)!r}, "w").close()'],
                    [nix, 'store', 'ping', '--store',
                     'unix://' + str(root / 'prepare-daemon.sock')], 30))
            self.assertFalse(denied.exists(), "capacity denial must precede worker launch")
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
