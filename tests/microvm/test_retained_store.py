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
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        nix = shutil.which("nix")
        shell_executable = shutil.which("bash")
        assert nix is not None and shell_executable is not None
        shell = Path(shell_executable).resolve()
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
            run("copy", "--from", "local?read-only=true",
                "--to", f"local?root={lower}", "--no-check-sigs", package)
            database = lower / "nix/var/nix/db/db.sqlite"
            before = hashlib.sha256(database.read_bytes()).digest()
            marker = root / "host-only"
            marker.touch()
            outputs = []
            expression = ""
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
            execution_spec = importlib.util.spec_from_file_location(
                "execution", ROOT / "deploy/microvm/execution.py")
            assert execution_spec is not None and execution_spec.loader is not None
            execution = importlib.util.module_from_spec(execution_spec)
            execution_spec.loader.exec_module(execution)
            daemon_environment = environment.copy()
            daemon_environment["NIX_DAEMON_SOCKET_PATH"] = str(root / "prepare-daemon.sock")
            daemon_environment["NIX_CONFIG"] += f"store = local?root={lower}\n"
            daemon_environment["TELCHAR_MICROVM_RETAINED_ROOT"] = str(lower)
            daemon_environment["TELCHAR_MICROVM_NIX"] = nix
            daemon_environment["TELCHAR_MICROVM_MAXIMUM_STORE_BYTES"] = str(2**40)
            daemon_environment["TELCHAR_MICROVM_MINIMUM_FREE_BYTES"] = "0"
            client = root / "worker.py"
            client.write_text(
                "import json, os, socket, subprocess\n"
                "binding=json.load(open(os.environ['TELCHAR_TEST_BINDING']))\n"
                "epoch=binding['assignment_epoch']\n"
                "channel = socket.socket(socket.AF_UNIX)\n"
                "channel.connect(os.environ['TELCHAR_MICROVM_JOB_SOCKET'])\n"
                "channel.sendall(b'prepared\\n')\n"
                "store = channel.makefile('rb').readline().decode().strip()\n"
                f"result = subprocess.run([{nix!r}, 'build', '--store', store, '--impure', "
                f"'--expr', {expression!r}.replace('retained-job-1', 'supervised-job-'+str(epoch)), '--no-link', '--print-out-paths'], capture_output=True)\n"
                "assert result.returncode == 0, result.stderr\n"
                "assert b'error:' not in result.stderr, result.stderr\n"
                "open(os.environ['TELCHAR_TEST_OUTPUTS'],'a').write(result.stdout.decode())\n")
            binding = root / 'binding.json'
            received = root / 'outputs'
            daemon_environment['TELCHAR_TEST_BINDING'] = str(binding)
            daemon_environment['TELCHAR_TEST_OUTPUTS'] = str(received)
            trust = root / 'trust'
            trust.write_text('fixture trust')
            state = execution.ExecutionState(
                [nix, 'daemon'], [sys.executable, str(client)],
                [nix, 'store', 'ping', '--store', 'unix://' + str(root / 'prepare-daemon.sock')],
                binding, trust)
            with patch.dict(os.environ, daemon_environment):
                for epoch in [1, 2]:
                    payload = {'version': 1, 'assignment_epoch': epoch,
                               'endpoint': 'wss://gateway.invalid/microvm',
                               'address': '127.0.0.1:443', 'intent_id': 'retained-worker',
                               'bearer': ('a' if epoch == 1 else 'b') * 43,
                               "trace_context": {"traceparent": None, "tracestate": None}, "lifetime_seconds": 30}
                    self.assertTrue(state.start_hook({'microvmId': 'provider-exact',
                                                      'runHookPayload': json.dumps(payload)}))
                    state.join()
                    self.assertEqual(state.status(), 'succeeded')
                    self.assertFalse(binding.exists())
                    self.assertTrue(state.report()['reset_complete'])
                    self.assertEqual(state.report()['assignment_epoch'], epoch)
            built = received.read_text().splitlines()
            self.assertEqual(len(set(built)), 2)
            for output in built:
                self.assertFalse((lower / 'nix/store' / Path(output).name).exists())
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
