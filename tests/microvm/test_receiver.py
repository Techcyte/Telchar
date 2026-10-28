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
            derivation = "/nix/store/" + "a" * 32 + "-build.drv"
            output = "/nix/store/" + "c" * 32 + "-output"
            def byte_list(value):
                return list(value.encode())
            configuration.write_text(json.dumps({
                "execution": "test", "microvmId": "instance", "credential": "a" * 64,
                "directory": directory, "store_uri": "unix:///nonexistent/socket",
                "manifest": {
                    "derivation_path": derivation,
                    "build": {"derivation_path": byte_list(derivation),
                              "outputs": [{"name": byte_list("out"), "path": byte_list(output), "hash_algorithm": [], "hash": []}],
                              "input_sources": [], "system": "x86_64-linux", "required_system_features": [],
                              "builder": byte_list("/bin/sh"), "arguments": [],
                              "environment": [[byte_list("out"), byte_list(output)]]},
                    "paths": [{"path": "/nix/store/" + "b" * 32 + "-input", "nar_hash": "0" * 64, "nar_size": 42, "references": [], "deriver": None, "content_address": None}], "outputs": [output],
                },
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
                        response = b""
                        while b"\r\n\r\n" not in response:
                            response += tls.recv(1)
                        if identity == "test":
                            first = tls.recv(1)
                            if not first:
                                stdout, stderr = receiver.communicate(timeout=5)
                                self.fail(f"receiver stopped before manifest: {stderr.decode()} {stdout.decode()}")
                            self.assertEqual(first, b"\x82", "manifest must be binary WebSocket message")
                            length = tls.recv(1)[0] & 0x7f
                            if length == 126:
                                length = int.from_bytes(tls.recv(2), "big")
                            elif length == 127:
                                length = int.from_bytes(tls.recv(8), "big")
                            frame = b""
                            while len(frame) < length:
                                frame += tls.recv(length - len(frame))
                            self.assertIn(derivation.encode(), frame)
                        return response.split(b"\r\n", 1)[0]
                self.assertEqual(exchange("foreign"), b"HTTP/1.1 403 Forbidden")
                self.assertEqual(exchange("test"), b"HTTP/1.1 101 Switching Protocols")
                stdout, stderr = receiver.communicate(timeout=5)
                self.assertNotEqual(receiver.returncode, 0, "disconnected transfer must fail")
                self.assertIn("execution authentication rejected", stderr.decode())
                self.assertIn("UnexpectedEof", stderr.decode())
                self.assertIn("peer closed connection without sending TLS close_notify", stderr.decode())
                self.assertEqual(stdout.decode().splitlines(), ["execution authentication accepted"])
            finally:
                if receiver.poll() is None:
                    receiver.kill()
                receiver.communicate()


if __name__ == "__main__":
    unittest.main()
