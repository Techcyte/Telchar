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

observation_spec = importlib.util.spec_from_file_location("process_observation", Path(__file__).with_name("process_observation.py"))
assert observation_spec is not None and observation_spec.loader is not None
observation = importlib.util.module_from_spec(observation_spec)
observation_spec.loader.exec_module(observation)

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
        self.proc = self.enterContext(observation.ancestor_proc(self.root))
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
                            f"pathlib.Path({str(self.root / 'daemon.pid')!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
                            "time.sleep(30)\n")

    def ready(self):
        return self.command("ready", "import pathlib, sys\n"
                            f"sys.exit(0 if pathlib.Path({str(self.root / 'daemon.pid')!r}).exists() else 1)\n")

    def process_stopped(self, marker):
        pid = int(marker.read_text())
        # A killed grandchild may remain a zombie until the host reaps it.
        try:
            return Path(f"/proc/{pid}/stat").read_text().split()[2] in {"Z", "X"}
        except FileNotFoundError:
            return True

    def assert_stopped(self, marker):
        self.assertTrue(self.process_stopped(marker), f"process {marker.read_text()} still running")

    def assert_execution_stopped(self, markers, deadline):
        # Namespace teardown stops every member, but has no member completion order.
        while not all(self.process_stopped(marker) for marker in markers):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(.01, remaining))
        for marker in markers:
            self.assert_stopped(marker)

    def test_cleanup_waits_for_daemon_after_child_stops(self):
        with subprocess.Popen([sys.executable, "-c", "pass"]) as child, \
                subprocess.Popen([sys.executable, "-c", "import time; time.sleep(.15)"]) as daemon:
            child.wait(timeout=1)
            markers = [self.root / "child.pid", self.root / "daemon.pid"]
            for marker, process in zip(markers, (child, daemon)):
                marker.write_text(str(process.pid))
            self.assert_execution_stopped(markers, time.monotonic() + 1)

    def test_cleanup_rejects_surviving_daemon_at_original_deadline(self):
        with subprocess.Popen([sys.executable, "-c", "pass"]) as child, \
                subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"]) as daemon:
            try:
                child.wait(timeout=1)
                markers = [self.root / "child.pid", self.root / "daemon.pid"]
                for marker, process in zip(markers, (child, daemon)):
                    marker.write_text(str(process.pid))
                with self.assertRaisesRegex(AssertionError, f"process {daemon.pid} still running"):
                    self.assert_execution_stopped(markers, time.monotonic() + .05)
            finally:
                daemon.kill()

    def test_deadline_kills_worker_descendant_and_daemon(self):
        child = self.command("child", "import os, pathlib, time\n"
                             f"pathlib.Path({str(self.root / 'child.pid')!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
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
                             f"pathlib.Path({str(self.root / 'escaped.pid')!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
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
                              f"    pathlib.Path({str(marker)!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
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
                              f"    pathlib.Path({str(marker)!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
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
                self.assert_execution_stopped(
                    [marker, self.root / "daemon.pid"], time.monotonic() + 1)
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

    def test_startup_diagnostics_are_bounded_and_exclude_command_output(self):
        import json
        for outcome in ("success", "daemon-exit", "readiness-deadline"):
            with self.subTest(outcome=outcome):
                daemon = self.command("diagnostic-daemon", "import time\ntime.sleep(30)\n")
                if outcome == "daemon-exit":
                    daemon = self.command("diagnostic-daemon", "raise SystemExit(17)\n")
                ready = self.command("diagnostic-ready", "raise SystemExit(" +
                                     ("1" if outcome == "readiness-deadline" else "0") + ")\n")
                worker = self.command("diagnostic-worker", "import sys\nprint('PRIVATE_MARKER', file=sys.stderr)\n")
                command = self.command("diagnostic-supervisor", "import runpy\n"
                                       f"execution = runpy.run_path({str(ROOT / 'deploy/microvm/execution.py')!r})\n"
                                       f"raise SystemExit(0 if execution['supervise']({daemon!r}, {worker!r}, {ready!r}, .5) else 1)\n")
                environment = os.environ.copy()
                environment["TELCHAR_MICROVM_DIAGNOSTICS"] = "1"
                result = subprocess.run(command, env=environment, capture_output=True, timeout=3)
                self.assertEqual(result.returncode, 0 if outcome == "success" else 1)
                self.assertEqual(result.stderr, b"")
                events = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertGreater(len(events), 0)
                self.assertLessEqual(len(events), 12)
                self.assertTrue(all(set(event) == {"event", "stage", "outcome"} for event in events))
                self.assertTrue(all(event["event"] == "microvm.execution.stage" for event in events))
                self.assertIn({"event": "microvm.execution.stage", "stage": "namespace", "outcome": "ready"}, events)
                outcomes = {outcome, "deadline"} if outcome == "readiness-deadline" else {outcome}
                self.assertTrue(any(event['stage'] == 'execution' and event['outcome'] in outcomes for event in events), events)
                self.assertNotIn(b"PRIVATE_MARKER", result.stdout)

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
                             f"pathlib.Path({str(self.root / 'ready.pid')!r}).write_text(pathlib.Path({str(self.proc / 'self/stat')!r}).read_text().split()[0])\n"
                             "time.sleep(30)\n")
        worker = self.command("worker", "raise SystemExit(0)\n")
        started = time.monotonic()
        self.assertFalse(execution.supervise(self.daemon(), worker, ready, .3))
        self.assertLess(time.monotonic() - started, 1.5)
        self.assert_stopped(self.root / "ready.pid")
        self.assert_stopped(self.root / "daemon.pid")

    def test_worker_exit_before_prepare_stops_execution_promptly(self):
        from unittest.mock import patch
        retained = self.root / 'retained'
        (retained / 'nix/store').mkdir(parents=True)
        (retained / 'nix/var/nix').mkdir(parents=True)
        worker = self.command('worker', 'raise SystemExit(1)\n')
        environment = {'TELCHAR_MICROVM_RETAINED_ROOT': str(retained),
                       'TELCHAR_MICROVM_MAXIMUM_STORE_BYTES': '1048576',
                       'TELCHAR_MICROVM_MINIMUM_FREE_BYTES': '0',
                       'PYTHONPATH': str(ROOT / 'deploy/microvm')}
        with patch.dict(os.environ, environment):
            started = time.monotonic()
            self.assertFalse(execution.supervise(self.daemon(), worker, self.ready(), 3))
            self.assertLess(time.monotonic() - started, 1.5)
        self.assert_stopped(self.root / 'daemon.pid')

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
