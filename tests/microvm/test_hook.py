"""Runtime binding and real process hook contracts."""
import concurrent.futures
import http.client
import importlib.util
import json
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


class HookTests(unittest.TestCase):
    def test_exact_hook_runs_once_and_conflicting_payload_rejects(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binding = root / "execution.json"
            marker = root / "worker-count"
            daemon = [sys.executable, "-c", "import time; time.sleep(30)"]
            ready = [sys.executable, "-c", "raise SystemExit(0)"]
            worker = [sys.executable, "-c", f"import pathlib; pathlib.Path({str(marker)!r}).open('a').write('run\\n')"]
            state = execution.ExecutionState(daemon, worker, ready, binding)
            self.assertFalse(binding.exists())
            self.assertFalse(marker.exists())
            payload = {"version": 1, "endpoint": "wss://gateway.invalid/microvm", "address": "127.0.0.1:443",
                       "intent_id": "intent", "bearer": "a" * 43, "lifetime_seconds": 2}
            hook = {"microvmId": "microvm-exact", "runHookPayload": json.dumps(payload)}
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                self.assertTrue(all(pool.map(state.start_hook, [hook] * 4)))
            state.join()
            self.assertEqual(state.status(), "succeeded")
            self.assertEqual(marker.read_text(), "run\n")
            stored = json.loads(binding.read_text())
            self.assertEqual(stored, {key: value for key, value in payload.items() if key != "version"} | {"provider_id": "microvm-exact"})
            self.assertEqual(binding.stat().st_mode & 0o777, 0o600)
            payload["intent_id"] = "other"
            self.assertFalse(state.start_hook({**hook, "runHookPayload": json.dumps(payload)}))
            self.assertFalse(state.start_hook({**hook, "microvmId": "microvm-other"}))
            self.assertEqual(marker.read_text(), "run\n")

    def test_termination_cancels_active_execution_without_relaunch(self):
        with tempfile.TemporaryDirectory() as directory:
            binding = Path(directory) / "execution.json"
            marker = Path(directory) / "worker-started"
            state = execution.ExecutionState(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                [sys.executable, "-c", f"import pathlib,time; pathlib.Path({str(marker)!r}).touch(); time.sleep(30)"],
                [sys.executable, "-c", "raise SystemExit(0)"], binding)
            payload = {"version": 1, "endpoint": "wss://gateway.invalid/microvm", "address": "127.0.0.1:443",
                       "intent_id": "intent", "bearer": "a" * 43, "lifetime_seconds": 10}
            hook = {"microvmId": "exact", "runHookPayload": json.dumps(payload)}
            self.assertTrue(state.start_hook(hook))
            deadline = time.monotonic() + 2
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            started = time.monotonic()
            runtime_spec = importlib.util.spec_from_file_location("runtime", ROOT / "deploy/microvm/runtime.py")
            assert runtime_spec is not None and runtime_spec.loader is not None
            runtime = importlib.util.module_from_spec(runtime_spec)
            runtime_spec.loader.exec_module(runtime)
            server = runtime.make_server(("127.0.0.1", 0), state, lambda: True)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                try:
                    connection.request("POST", runtime.PREFIX + "terminate", "{}")
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    response.read()
                finally:
                    connection.close()
                state.thread.join(timeout=1)
                self.assertFalse(state.thread.is_alive(), "terminate hook must stop execution")
            finally:
                state.terminate()
                state.join()
                server.shutdown()
                server.server_close()
                thread.join()
            self.assertLess(time.monotonic() - started, 1.5)
            self.assertEqual(state.status(), "failed")
            self.assertTrue(state.start_hook(hook))
            self.assertEqual(state.status(), "failed")

    def test_invalid_hook_cannot_write_binding_or_start_processes(self):
        with tempfile.TemporaryDirectory() as directory:
            binding = Path(directory) / "execution.json"
            state = execution.ExecutionState([], [], [], binding)
            for payload in [None, {}, {"microvmId": "id", "runHookPayload": "{}"},
                            {"microvmId": "id", "runHookPayload": " " * 16385}]:
                with self.assertRaises(ValueError):
                    state.start_hook(payload)
                self.assertFalse(binding.exists())
            self.assertEqual(state.status(), "idle")

    def test_binding_destination_never_overwrites_existing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            binding = Path(directory) / "execution.json"
            binding.write_text("operator-owned")
            state = execution.ExecutionState([], [], [], binding)
            payload = {"version": 1, "endpoint": "wss://gateway.invalid/microvm", "address": "127.0.0.1:443",
                       "intent_id": "intent", "bearer": "a" * 43, "lifetime_seconds": 2}
            with self.assertRaises(FileExistsError):
                state.start_hook({"microvmId": "exact", "runHookPayload": json.dumps(payload)})
            self.assertEqual(binding.read_text(), "operator-owned")
            self.assertEqual(state.status(), "idle")


if __name__ == "__main__":
    unittest.main()
