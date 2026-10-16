"""Real process contracts for image-local execution supervision."""
import contextlib
import importlib.util
import os
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("microvm_execution", ROOT / "deploy/microvm/execution.py")
assert spec is not None and spec.loader is not None
execution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(execution)


class ExecutionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.addCleanup(self.cleanup_processes)

    def cleanup_processes(self):
        for marker in self.root.glob("*.pid"):
            with contextlib.suppress(ProcessLookupError):
                os.kill(int(marker.read_text()), signal.SIGKILL)

    def command(self, name, body):
        script = self.root / (name + ".py")
        script.write_text(body)
        return [sys.executable, str(script)]

    def daemon(self):
        return self.command("daemon", "import os, pathlib, time\n"
                            f"pathlib.Path({str(self.root / 'daemon.pid')!r}).write_text(str(os.getpid()))\n"
                            "time.sleep(30)\n")

    def ready(self):
        return self.command("ready", "import pathlib, sys\n"
                            f"sys.exit(0 if pathlib.Path({str(self.root / 'daemon.pid')!r}).exists() else 1)\n")

    def assert_stopped(self, marker):
        pid = int(marker.read_text())
        # A killed grandchild may remain a zombie until the host reaps it.
        status = Path(f"/proc/{pid}/stat")
        if status.exists():
            self.assertEqual(status.read_text().split()[2], "Z", f"process {pid} still running")

    def test_deadline_kills_worker_descendant_and_daemon(self):
        child = self.command("child", "import os, pathlib, time\n"
                             f"pathlib.Path({str(self.root / 'child.pid')!r}).write_text(str(os.getpid()))\n"
                             "time.sleep(30)\n")
        worker = self.command("worker", "import subprocess, time\n"
                              f"subprocess.Popen({child!r})\n"
                              "time.sleep(30)\n")
        started = time.monotonic()
        result = execution.supervise(self.daemon(), worker, self.ready(), 1)
        self.assertFalse(result)
        self.assertLess(time.monotonic() - started, 2)
        self.assert_stopped(self.root / "child.pid")
        self.assert_stopped(self.root / "daemon.pid")

    def test_success_stops_daemon(self):
        worker = self.command("worker", "raise SystemExit(0)\n")
        self.assertTrue(execution.supervise(self.daemon(), worker, self.ready(), 2))
        self.assert_stopped(self.root / "daemon.pid")

    def test_readiness_failure_never_starts_worker(self):
        marker = self.root / "worker-started"
        worker = self.command("worker", f"import pathlib\npathlib.Path({str(marker)!r}).touch()\n")
        ready = self.command("unready", "raise SystemExit(1)\n")
        self.assertFalse(execution.supervise(self.daemon(), worker, ready, .3))
        self.assertFalse(marker.exists())
        self.assert_stopped(self.root / "daemon.pid")

    def test_hanging_readiness_obeys_execution_deadline(self):
        ready = self.command("ready", "import os, pathlib, time\n"
                             f"pathlib.Path({str(self.root / 'ready.pid')!r}).write_text(str(os.getpid()))\n"
                             "time.sleep(30)\n")
        worker = self.command("worker", "raise SystemExit(0)\n")
        started = time.monotonic()
        self.assertFalse(execution.supervise(self.daemon(), worker, ready, .3))
        self.assertLess(time.monotonic() - started, 1.5)
        self.assert_stopped(self.root / "ready.pid")
        self.assert_stopped(self.root / "daemon.pid")

    def test_worker_failure_stops_daemon(self):
        worker = self.command("worker", "raise SystemExit(1)\n")
        self.assertFalse(execution.supervise(self.daemon(), worker, self.ready(), 2))
        self.assert_stopped(self.root / "daemon.pid")

    def test_daemon_exit_aborts_worker_before_deadline(self):
        daemon = self.command("daemon", "import time\ntime.sleep(.3)\nraise SystemExit(1)\n")
        ready = self.command("ready", "raise SystemExit(0)\n")
        worker = self.command("worker", "import time\ntime.sleep(30)\n")
        started = time.monotonic()
        self.assertFalse(execution.supervise(daemon, worker, ready, 3))
        self.assertLess(time.monotonic() - started, 1.5)


if __name__ == "__main__":
    unittest.main()
