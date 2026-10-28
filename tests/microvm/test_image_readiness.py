"""Static image prerequisites through the actual lifecycle HTTP process."""
import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class ImageReadinessTests(unittest.TestCase):
    def test_readiness_requires_artifacts_without_executing_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "executed"
            commands = []
            for name in ("daemon", "ready", "worker"):
                command = root / name
                command.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nPath({str(marker)!r}).touch()\n")
                command.chmod(0o700)
                commands.append(command)
            trust = root / "trust.der"
            binding = root / "binding"
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            process = subprocess.Popen([
                sys.executable, str(ROOT / "deploy/microvm/execution.py"),
                "--listen", "127.0.0.1", "--port", str(port),
                "--daemon", str(commands[0]), "--ready", str(commands[1]),
                "--worker", str(commands[2]), "--trust", str(trust),
                "--binding", str(binding), "--store", "unix:///isolated-nix.sock",
            ], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})

            def request(route, body="{}"):
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
                try:
                    connection.request("POST", "/aws/lambda-microvms/runtime/v1/" + route, body)
                    response = connection.getresponse()
                    response.read()
                    return response.status
                finally:
                    connection.close()

            try:
                deadline = time.monotonic() + 3
                while True:
                    try:
                        self.assertEqual(request("ready"), 503)
                        break
                    except OSError:
                        if time.monotonic() >= deadline or process.poll() is not None:
                            self.fail("image listener did not start")
                        time.sleep(.02)
                payload = {"version": 1, "endpoint": "wss://gateway.invalid/microvm",
                           "address": "127.0.0.1:443", "intent_id": "intent",
                           "bearer": "a" * 43, "lifetime_seconds": 1}
                hook = json.dumps({"microvmId": "exact", "runHookPayload": json.dumps(payload)})
                self.assertEqual(request("run", hook), 400)
                self.assertFalse(binding.exists())
                trust.write_bytes(b"runtime-trust")
                self.assertEqual(request("ready"), 200)
                for command in commands:
                    command.chmod(0o600)
                    self.assertEqual(request("ready"), 503)
                    command.chmod(0o700)
                trust.unlink()
                self.assertEqual(request("validate"), 503)
                for content in (b"", b"x" * 65537):
                    trust.write_bytes(content)
                    self.assertEqual(request("ready"), 503)
                    self.assertEqual(request("run", hook), 400)
                    self.assertFalse(binding.exists())
                trust.write_bytes(b"runtime-trust")
                binding.write_text("retained binding")
                self.assertEqual(request("ready"), 503)
                binding.unlink()
                self.assertEqual(request("ready"), 200)
                self.assertFalse(marker.exists(), "readiness executed an image command")
                self.assertFalse(binding.exists())
            finally:
                process.terminate()
                try:
                    stdout, stderr = process.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    stdout, stderr = process.communicate()
                self.assertEqual(stdout, b"")
                self.assertEqual(stderr, b"")


if __name__ == "__main__":
    unittest.main()
