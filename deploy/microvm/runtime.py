#!/usr/bin/env python3
"""Experimental AWS lifecycle adapter; deliberately not an arbitrary job API."""

import argparse
import json
import os
import signal
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PREFIX = "/aws/lambda-microvms/runtime/v1/"
MAX_BODY = 18432


class ProbeState:
    def __init__(self, command, timeout=300):
        self.command = command
        self.timeout = timeout
        self.lock = threading.Lock()
        self.identity = None
        self.result = "idle"
        self.thread = None
        self.cancelled = threading.Event()

    def status(self):
        with self.lock:
            return self.result

    def start_hook(self, payload):
        identity = payload.get("microvmId")
        if not isinstance(identity, str) or not identity or len(identity) > 256 or payload.get("runHookPayload", "") != "":
            raise ValueError("invalid probe hook")
        return self.start(identity)

    def start(self, identity):
        with self.lock:
            if self.identity is not None:
                return self.identity == identity
            self.identity = identity
            self.result = "running"
            self.thread = threading.Thread(target=self._execute, daemon=True)
            self.thread.start()
            return True

    def terminate(self):
        self.cancelled.set()

    def _execute(self):
        result = "failed"
        try:
            # No shell or request-supplied arguments. Bound execution and kill the
            # entire probe process group if it times out.
            process = subprocess.Popen(self.command, start_new_session=True)
            try:
                deadline = time.monotonic() + self.timeout
                while not self.cancelled.is_set() and time.monotonic() < deadline:
                    code = process.poll()
                    if code is not None:
                        if code == 0:
                            result = "succeeded"
                        break
                    self.cancelled.wait(min(.025, max(0, deadline - time.monotonic())))
                else:
                    raise subprocess.TimeoutExpired(self.command, self.timeout)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        except OSError:
            pass
        with self.lock:
            self.result = result
        print(json.dumps({"event": "microvm.probe.completed", "outcome": result}), flush=True)

    def join(self):
        if self.thread is not None:
            self.thread.join()


def make_server(address, state, daemon_ready):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format, *args):
            # Requests may contain runtime secrets; never log paths or bodies.
            pass

        def reply(self, code):
            data = json.dumps({"status": state.status()}).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)
            self.close_connection = True

        def do_POST(self):
            self.connection.settimeout(5)
            if self.path not in {PREFIX + name for name in ("ready", "validate", "run", "suspend", "resume", "terminate")}:
                self.reply(404)
                return
            if self.headers.get("Transfer-Encoding"):
                self.reply(400)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if length < 0 or length > MAX_BODY:
                    self.reply(413)
                    return
                body = self.rfile.read(length)
                if len(body) != length:
                    self.reply(400)
                    return
                payload = json.loads(body or b"{}")
                if not isinstance(payload, dict):
                    raise ValueError("object required")
            except (ValueError, OSError):
                self.reply(400)
                return
            if self.path == PREFIX + "run":
                try:
                    accepted = state.start_hook(payload)
                except (ValueError, OSError):
                    self.reply(400)
                    return
                self.reply(200 if accepted else 409)
            elif self.path in (PREFIX + "ready", PREFIX + "validate", PREFIX + "resume"):
                self.reply(409 if state.status() == "running" else (200 if daemon_ready() else 503))
            elif self.path == PREFIX + "suspend":
                self.reply(409 if state.status() == "running" else 200)
            else:
                state.terminate()
                self.reply(200)

    server = ThreadingHTTPServer(address, Handler)
    server.daemon_threads = True
    return server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()

    def daemon_ready():
        try:
            result = subprocess.run(
                ["nix", "--offline", "store", "ping", "--store", "daemon"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                timeout=2,
                check=False,
            )
            return result.returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            return False

    # AWS routes the authenticated MicroVM endpoint to this application port.
    # This is an image-local lifecycle API, not a host-network deployment.
    make_server(("0.0.0.0", 8080), ProbeState([args.probe]), daemon_ready).serve_forever()  # noqa: S104


if __name__ == "__main__":
    main()
