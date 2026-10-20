"""Real process contracts for image-local execution supervision."""
import contextlib
import importlib.util
import os
import signal
import subprocess
import sys
import tempfile
import threading
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
                            f"pathlib.Path({str(self.root / 'daemon.pid')!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
                            "time.sleep(30)\n")

    def ready(self):
        return self.command("ready", "import pathlib, sys\n"
                            f"sys.exit(0 if pathlib.Path({str(self.root / 'daemon.pid')!r}).exists() else 1)\n")

    def assert_stopped(self, marker):
        pid = int(marker.read_text())
        # A killed grandchild may remain a zombie until the host reaps it.
        status = Path(f"/proc/{pid}/stat")
        if status.exists():
            self.assertIn(status.read_text().split()[2], {"Z", "X"}, f"process {pid} still running")

    def test_deadline_kills_worker_descendant_and_daemon(self):
        child = self.command("child", "import os, pathlib, time\n"
                             f"pathlib.Path({str(self.root / 'child.pid')!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
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

    def test_deadline_stops_descendant_that_creates_session(self):
        child = self.command("escaped", "import os, pathlib, time\n"
                             "os.setsid()\n"
                             f"pathlib.Path({str(self.root / 'escaped.pid')!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
                             "time.sleep(5)\n")
        worker = self.command("worker", "import subprocess, time\n"
                              f"subprocess.Popen({child!r})\n"
                              "time.sleep(5)\n")
        started = time.monotonic()
        self.assertFalse(execution.supervise(self.daemon(), worker, self.ready(), 1))
        self.assertLess(time.monotonic() - started, 2)
        self.assert_stopped(self.root / "daemon.pid")
        self.assert_stopped(self.root / "escaped.pid")

    def test_cancellation_stops_escaped_descendant_before_deadline(self):
        marker = self.root / "escaped.pid"
        worker = self.command("worker", "import os, pathlib, time\n"
                              "if os.fork() == 0:\n"
                              "    os.setsid()\n"
                              f"    pathlib.Path({str(marker)!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
                              "time.sleep(5)\n")
        cancelled = threading.Event()
        result = []
        thread = threading.Thread(target=lambda: result.append(
            execution.supervise(self.daemon(), worker, self.ready(), 10, cancelled)))
        thread.start()
        try:
            deadline = time.monotonic() + 2
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists(), "escaped descendant never started")
            started = time.monotonic()
            cancelled.set()
            thread.join(timeout=2)
            self.assertFalse(thread.is_alive(), "cancellation waited for execution deadline")
            self.assertEqual(result, [False])
            self.assertLess(time.monotonic() - started, 2)
            self.assert_stopped(marker)
            self.assert_stopped(self.root / "daemon.pid")
        finally:
            cancelled.set()
            thread.join(timeout=12)

    def test_supervisor_death_stops_escaped_descendant(self):
        self.assert_process_loss_stops_execution(False)

    def test_namespace_helper_death_stops_escaped_descendant(self):
        self.assert_process_loss_stops_execution(True)

    def assert_process_loss_stops_execution(self, kill_helper):
        marker = self.root / "escaped.pid"
        worker = self.command("worker", "import os, pathlib, time\n"
                              "if os.fork() == 0:\n"
                              "    os.setsid()\n"
                              f"    pathlib.Path({str(marker)!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
                              "time.sleep(5)\n")
        command = self.command("supervisor", "import runpy\n"
                               f"execution = runpy.run_path({str(ROOT / 'deploy/microvm/execution.py')!r})\n"
                               f"execution['supervise']({self.daemon()!r}, {worker!r}, {self.ready()!r}, 10)\n")
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
            try:
                deadline = time.monotonic() + 2
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(marker.exists(), "escaped descendant never started")
                if kill_helper:
                    children = Path(f"/proc/{process.pid}/task/{process.pid}/children").read_text().split()
                    self.assertEqual(len(children), 1)
                    os.kill(int(children[0]), signal.SIGKILL)
                else:
                    process.kill()
                stdout, stderr = process.communicate(timeout=2)
                self.assertEqual(stdout, b"")
                self.assertEqual(stderr, b"")
                deadline = time.monotonic() + 1
                status = Path(f"/proc/{int(marker.read_text())}/stat")
                while status.exists() and time.monotonic() < deadline:
                    if status.read_text().split()[2] in {"Z", "X"}:
                        break
                    time.sleep(.01)
                self.assert_stopped(marker)
                self.assert_stopped(self.root / "daemon.pid")
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=2)

    def test_missing_namespace_capability_never_starts_commands(self):
        marker = self.root / "command-started"
        command = self.command("command", f"import pathlib\npathlib.Path({str(marker)!r}).touch()\n")
        probe = self.command("capability", "import pathlib, runpy\n"
                             "status = pathlib.Path('/proc/self/status').read_text().splitlines()\n"
                             "assert int(next(line.split()[1] for line in status if line.startswith('CapEff:')), 16) == 0\n"
                             f"execution = runpy.run_path({str(ROOT / 'deploy/microvm/execution.py')!r})\n"
                             f"assert execution['supervise']({command!r}, {command!r}, {command!r}, 2) is False\n")
        result = subprocess.run(
            ["setpriv", "--bounding-set=-all", "--inh-caps=-all", "--ambient-caps=-all", *probe],
            capture_output=True, timeout=3)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"")
        self.assertFalse(marker.exists(), "commands ran without namespace containment")

    def test_execution_procfs_matches_process_namespace(self):
        worker = self.command("worker", "import os, pathlib\n"
                              "pid = os.getpid()\n"
                              "assert int(pathlib.Path('/proc/self/stat').read_text().split()[0]) == pid\n"
                              "assert pathlib.Path('/proc/' + str(pid) + '/ns/mnt').exists()\n")
        self.assertTrue(execution.supervise(self.daemon(), worker, self.ready(), 2),
                        "execution procfs does not describe its PID namespace")
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
                             f"pathlib.Path({str(self.root / 'ready.pid')!r}).write_text(pathlib.Path('/proc/self/stat').read_text().split()[0])\n"
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
