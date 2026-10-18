"""Supervision contracts with a real private Nix daemon and local build client.

Sandboxing is disabled in this non-root fixture; this is not image containment proof.
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("microvm_execution", ROOT / "deploy/microvm/execution.py")
assert spec is not None and spec.loader is not None
execution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(execution)


class NixExecutionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="telchar-supervised-nix-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name in ("store", "state", "log", "config", "tmp"):
            (self.root / name).mkdir()
        self.socket = self.root / "daemon.sock"
        self.environment = {
            "PATH": os.environ["PATH"], "HOME": str(self.root),
            "NIX_STORE_DIR": str(self.root / "store"),
            "NIX_STATE_DIR": str(self.root / "state"),
            "NIX_LOG_DIR": str(self.root / "log"),
            "NIX_CONF_DIR": str(self.root / "config"),
            "NIX_DAEMON_SOCKET_PATH": str(self.socket),
            "NIX_USER_CONF_FILES": "/dev/null", "TMPDIR": str(self.root / "tmp"),
            "NIX_CONFIG": "experimental-features = nix-command\nbuild-users-group =\nsandbox = false\nsubstituters =\ntrusted-users = *\n",
        }
        self.nix = shutil.which("nix")
        self.daemon = shutil.which("nix-daemon")
        self.assertIsNotNone(self.nix, "flake development shell required")
        self.assertIsNotNone(self.daemon, "flake development shell required")
        initialized = subprocess.run(["nix-store", "--store", "local", "--init"],
                                     env=self.environment, capture_output=True)
        self.assertEqual(initialized.returncode, 0, initialized.stderr.decode())
        self.assertEqual(initialized.stderr, b"")

    def command(self, name, body):
        path = self.root / (name + ".py")
        path.write_text("import os, pathlib, subprocess\n" +
                        f"environment = {self.environment!r}\n" + body)
        return [sys.executable, str(path)]

    def run_build(self, body):
        daemon_log = self.root / "daemon.log"
        daemon = self.command("daemon", f"pathlib.Path({str(self.root / 'daemon.pid')!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
                              f"log = os.open({str(daemon_log)!r}, os.O_CREAT | os.O_WRONLY, 0o600)\n"
                              "os.dup2(log, 2)\n"
                              f"os.execve({self.daemon!r}, [{self.daemon!r}], environment)\n")
        ready = self.command("ready", f"result = subprocess.run([{self.nix!r}, '--store', 'unix://{self.socket}', 'store', 'ping'], env=environment, capture_output=True)\n"
                             "raise SystemExit(result.returncode)\n")
        expression = ('derivation { name = "supervised-build"; system = builtins.currentSystem; '
                      'builder = "/bin/sh"; args = [ "-c" ' + json.dumps(body) + ' ]; }')
        worker = self.command("worker", f"result = subprocess.run([{self.nix!r}, '--store', 'unix://{self.socket}', 'build', '--impure', '--no-link', '--print-out-paths', '--expr', {expression!r}], env=environment, capture_output=True)\n"
                              f"pathlib.Path({str(self.root / 'build.stdout')!r}).write_bytes(result.stdout)\n"
                              f"pathlib.Path({str(self.root / 'build.stderr')!r}).write_bytes(result.stderr)\n"
                              "raise SystemExit(result.returncode)\n")
        result = execution.supervise(daemon, worker, ready, 10)
        pid = int((self.root / "daemon.pid").read_text())
        self.assertFalse(Path(f"/proc/{pid}").exists(), "supervised daemon must be reaped")
        for line in daemon_log.read_text().splitlines():
            self.assertRegex(line, r"^accepted connection from pid [0-9]+, user [^ ]+ \(trusted\)$")
        return result

    def test_real_build_success_stops_private_daemon(self):
        self.assertTrue(self.run_build('printf supervised-nix > "$out"'))
        output = Path((self.root / "build.stdout").read_text().strip())
        self.assertTrue(output.is_relative_to(self.root / "store"))
        self.assertEqual(output.read_bytes(), b"supervised-nix")
        diagnostic = (self.root / "build.stderr").read_text()
        self.assertIn("building ", diagnostic)
        self.assertNotIn("error:", diagnostic)
        self.assertNotIn("warning:", diagnostic)

    def test_real_build_failure_stops_private_daemon(self):
        self.assertFalse(self.run_build('echo supervised-build-failure >&2; exit 7'))
        self.assertEqual((self.root / "build.stdout").read_bytes(), b"")
        diagnostic = (self.root / "build.stderr").read_text()
        self.assertIn("supervised-build-failure", diagnostic)
        self.assertIn("exit code 7", diagnostic)
        self.assertNotIn("warning:", diagnostic)


if __name__ == "__main__":
    unittest.main()
