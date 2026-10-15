#!/usr/bin/env python3
"""Real TLS/WebSocket execution admission against the disposable receiver."""
import base64
import json
import os
import socket
import ssl
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


class ReceiverTests(unittest.TestCase):
    def test_rejects_wrong_execution_then_accepts_exact_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            certificate = root / "certificate.pem"
            key = root / "key.pem"
            generation = subprocess.run([
                "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                "-keyout", str(key), "-out", str(certificate), "-days", "1",
                "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost",
            ], capture_output=True)
            self.assertEqual(generation.returncode, 0, generation.stderr.decode())
            configuration = root / "execution.json"
            configuration.write_text(json.dumps({
                "execution": "test", "microvmId": "instance", "credential": "a" * 64,
            }))
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            receiver = subprocess.Popen([
                os.environ.get("MICROVM_RECEIVER", "target/debug/examples/microvm-receiver"),
                str(configuration), str(certificate), str(key), f"127.0.0.1:{port}",
            ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                context = ssl.create_default_context(cafile=str(certificate))
                def exchange(identity):
                    deadline = time.monotonic() + 5
                    while True:
                        try:
                            stream = socket.create_connection(("127.0.0.1", port), timeout=2)
                            break
                        except ConnectionRefusedError:
                            if receiver.poll() is not None:
                                stdout, stderr = receiver.communicate()
                                self.fail(f"receiver exited {receiver.returncode}: {stderr.decode()} {stdout.decode()}")
                            if time.monotonic() >= deadline:
                                raise
                            time.sleep(.05)
                    with context.wrap_socket(stream, server_hostname="localhost") as tls:
                        nonce = base64.b64encode(os.urandom(16)).decode()
                        tls.sendall((f"GET /fixture HTTP/1.1\r\nHost: localhost:{port}\r\n"
                                     "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                                     f"Sec-WebSocket-Key: {nonce}\r\nSec-WebSocket-Version: 13\r\n"
                                     f"Authorization: Bearer {'a' * 64}\r\n"
                                     f"X-Telchar-Execution: {identity}\r\nX-Telchar-Microvm: instance\r\n\r\n").encode())
                        return tls.recv(4096).split(b"\r\n", 1)[0]
                self.assertEqual(exchange("foreign"), b"HTTP/1.1 403 Forbidden")
                self.assertEqual(exchange("test"), b"HTTP/1.1 101 Switching Protocols")
                stdout, stderr = receiver.communicate(timeout=5)
                self.assertEqual(receiver.returncode, 0, stderr.decode())
                self.assertEqual(stderr.decode().splitlines(), ["execution authentication rejected"])
                self.assertEqual(stdout.decode().splitlines(), ["execution authentication accepted"])
            finally:
                if receiver.poll() is None:
                    receiver.kill()
                receiver.communicate()


if __name__ == "__main__":
    unittest.main()
