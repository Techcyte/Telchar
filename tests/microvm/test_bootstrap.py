#!/usr/bin/env python3
"""Execution bootstrap contracts for the disposable WSS fixture."""
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("bootstrap", Path(__file__).with_name("bootstrap.py"))
assert spec is not None and spec.loader is not None
bootstrap = importlib.util.module_from_spec(spec)
loader = spec.loader


class BootstrapTests(unittest.TestCase):
    def test_bootstrap_binds_exact_instance_and_protects_credentials(self):
        loader.exec_module(bootstrap)
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "execution.json"
            value = {"microvmId": "microvm-test", "runHookPayload": json.dumps({
                "endpoint": "wss://receiver.example/fixture", "credential": "a" * 64,
                "certificate": "abcd", "execution": "test-execution",
            })}
            bootstrap.write_bootstrap(value, destination)
            result = json.loads(destination.read_text())
            self.assertEqual(result["microvmId"], "microvm-test")
            self.assertEqual(result["execution"], "test-execution")
            self.assertEqual(os.stat(destination).st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                bootstrap.write_bootstrap(value, destination)

    def test_run_hook_writes_binding_before_starting_fixed_worker(self):
        loader.exec_module(bootstrap)
        import concurrent.futures
        import sys
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "execution.json"
            observed = Path(directory) / "observed.json"
            command = [sys.executable, "-c",
                       "import pathlib,sys; pathlib.Path(sys.argv[2]).write_bytes(pathlib.Path(sys.argv[1]).read_bytes())",
                       str(destination), str(observed)]
            state = bootstrap.ExecutionState(command, destination)
            hook = {"microvmId": "microvm-test", "runHookPayload": json.dumps({
                "endpoint": "wss://receiver.example/fixture", "credential": "a" * 64,
                "certificate": "abcd", "execution": "test-execution",
            })}
            import http.client
            import threading
            server = bootstrap.runtime.make_server(("127.0.0.1", 0), state, lambda: True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            def deliver(_):
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                try:
                    connection.request("POST", bootstrap.runtime.PREFIX + "run", json.dumps(hook))
                    response = connection.getresponse()
                    response.read()
                    return response.status
                finally:
                    connection.close()
            try:
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    results = list(pool.map(deliver, range(4)))
            finally:
                server.shutdown()
                server.server_close()
                thread.join()
            self.assertEqual(results, [200] * 4)
            state.join()
            self.assertEqual(state.status(), "succeeded")
            self.assertEqual(json.loads(observed.read_text())["microvmId"], "microvm-test")
            self.assertFalse(state.start_hook({**hook, "microvmId": "foreign"}))

    def test_worker_deadline_leaves_time_before_provider_termination(self):
        loader.exec_module(bootstrap)
        with tempfile.TemporaryDirectory() as directory:
            state = bootstrap.ExecutionState(["/bin/true"], Path(directory) / "execution.json")
            self.assertEqual(state.timeout, 540)

    def test_bootstrap_rejects_unencrypted_or_credential_bearing_endpoints(self):
        loader.exec_module(bootstrap)
        with tempfile.TemporaryDirectory() as directory:
            for endpoint in ["ws://receiver/fixture", "wss://user:secret@receiver/fixture", "file:///tmp/x"]:
                with self.assertRaises(ValueError):
                    bootstrap.write_bootstrap({"microvmId": "test", "runHookPayload": json.dumps({
                        "endpoint": endpoint, "credential": "a" * 64,
                        "certificate": "abcd", "execution": "test",
                    })}, Path(directory) / "execution.json")


if __name__ == "__main__":
    unittest.main()
