#!/usr/bin/env python3
"""Behavioral contracts for the experimental MicroVM lifecycle server."""
import concurrent.futures
import contextlib
import http.client
import importlib.util
import io
import json
import tempfile
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("microvm_runtime", ROOT / "deploy/microvm/runtime.py")
assert spec is not None and spec.loader is not None
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.marker = Path(self.directory.name) / "invocations"
        self.script = Path(self.directory.name) / "probe.py"
        self.script.write_text(
            "import pathlib, sys, time\n"
            "with pathlib.Path(sys.argv[1]).open('a') as f: f.write('run\\n')\n"
            "time.sleep(.1)\n"
        )
        import sys
        self.state = runtime.ProbeState([sys.executable, str(self.script), str(self.marker)], timeout=5)
        self.server = runtime.make_server(("127.0.0.1", 0), self.state, lambda: True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.close)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.state.join()

    def request(self, path, body=b"{}"):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        try:
            connection.request("POST", path, body, {"Content-Type": "application/json"})
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def run_payload(self, identity="mvm-test", payload=""):
        return json.dumps({"microvmId": identity, "runHookPayload": payload}).encode()

    def test_ready_does_not_execute_probe_before_snapshot(self):
        self.assertEqual(self.request(runtime.PREFIX + "ready")[0], 200)
        self.assertFalse(self.marker.exists())
        self.assertEqual(self.state.status(), "idle")

    def test_run_is_idempotent_under_concurrent_hook_delivery(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            results = list(pool.map(lambda _: self.request(runtime.PREFIX + "run", self.run_payload()), range(6)))
        self.assertTrue(all(status == 200 for status, _ in results))
        self.state.join()
        self.assertEqual(self.marker.read_text(), "run\n")
        self.assertEqual(self.state.status(), "succeeded")
        for route in ("ready", "validate", "suspend", "resume"):
            self.assertEqual(self.request(runtime.PREFIX + route)[0], 409, route)
        self.assertEqual(self.request(runtime.PREFIX + "run", self.run_payload("other"))[0], 409)

    def test_invalid_requests_cannot_start_probe(self):
        for body in [b"[]", b"null", b"{", b"{}", self.run_payload(payload="arbitrary command")]:
            self.assertEqual(self.request(runtime.PREFIX + "run", body)[0], 400)
        self.assertFalse(self.marker.exists())

    def test_oversize_body_is_rejected(self):
        self.assertEqual(self.request(runtime.PREFIX + "run", b" " * 20000)[0], 413)
        self.assertFalse(self.marker.exists())

    def test_active_probe_cannot_be_suspended_or_snapshotted(self):
        self.script.write_text("import time\ntime.sleep(.5)\n")
        self.assertEqual(self.request(runtime.PREFIX + "run", self.run_payload())[0], 200)
        self.assertEqual(self.request(runtime.PREFIX + "suspend")[0], 409)
        self.assertEqual(self.request(runtime.PREFIX + "ready")[0], 409)

    def test_failure_and_timeout_are_terminal(self):
        import sys
        for command, timeout in [([sys.executable, "-c", "raise SystemExit(1)"], 5),
                                 ([sys.executable, "-c", "import time; time.sleep(10)"], .05)]:
            state = runtime.ProbeState(command, timeout=timeout)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertTrue(state.start("test"))
                state.join()
            self.assertEqual(json.loads(output.getvalue()), {
                "event": "microvm.probe.completed", "outcome": "failed",
            })
            self.assertEqual(state.status(), "failed")
            self.assertTrue(state.start("test"))
            self.assertEqual(state.status(), "failed")

    def test_unknown_route_is_not_a_command_endpoint(self):
        self.assertEqual(self.request("/execute", b'{"command":"id"}')[0], 404)


if __name__ == "__main__":
    unittest.main()
