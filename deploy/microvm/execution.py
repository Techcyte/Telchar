"""Image-local fresh Nix daemon and worker lifetime supervision."""
import contextlib
import ipaddress
import json
import os
import signal
import subprocess
import threading
import time
from urllib.parse import urlsplit


class ExecutionState:
    def __init__(self, daemon_command, worker_command, ready_command, destination):
        self.daemon_command = daemon_command
        self.worker_command = worker_command
        self.ready_command = ready_command
        self.destination = destination
        self.lock = threading.Lock()
        self.binding = None
        self.result = "idle"
        self.thread = None

    def status(self):
        with self.lock:
            return self.result

    def start_hook(self, hook):
        if not isinstance(hook, dict):
            raise ValueError("invalid execution hook")
        identity = hook.get("microvmId")
        payload = hook.get("runHookPayload")
        if (not isinstance(identity, str) or not 1 <= len(identity) <= 256
                or not isinstance(payload, str) or len(payload.encode()) > 16384):
            raise ValueError("invalid execution hook")
        try:
            value = json.loads(payload)
        except json.JSONDecodeError as error:
            raise ValueError("invalid execution payload") from error
        fields = {"version", "endpoint", "address", "intent_id", "bearer", "lifetime_seconds"}
        if not isinstance(value, dict) or set(value) != fields or type(value["version"]) is not int or value["version"] != 1:
            raise ValueError("invalid execution schema")
        if any(not isinstance(value[key], str) for key in ("endpoint", "address", "intent_id", "bearer")):
            raise ValueError("invalid execution values")
        endpoint = urlsplit(value["endpoint"])
        if (endpoint.scheme != "wss" or not endpoint.hostname or endpoint.path != "/microvm"
                or endpoint.username is not None or endpoint.password is not None
                or endpoint.query or endpoint.fragment):
            raise ValueError("invalid execution endpoint")
        address = urlsplit("//" + value["address"])
        ipaddress.ip_address(address.hostname)
        if address.port is None or not 1 <= address.port <= 65535 or address.path or address.query or address.fragment or address.username is not None:
            raise ValueError("invalid execution address")
        if (not 1 <= len(value["intent_id"]) <= 128 or len(value["bearer"]) != 43
                or any(character not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_" for character in value["bearer"])
                or type(value["lifetime_seconds"]) is not int or not 1 <= value["lifetime_seconds"] <= 28800):
            raise ValueError("invalid execution limits")
        binding = {key: item for key, item in value.items() if key != "version"}
        binding["provider_id"] = identity
        with self.lock:
            if self.binding is not None:
                return self.binding == binding
            descriptor = os.open(self.destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w") as output:
                json.dump(binding, output)
            self.binding = binding
            self.result = "running"
            self.thread = threading.Thread(target=self._execute, args=(value["lifetime_seconds"],), daemon=True)
            self.thread.start()
            return True

    def _execute(self, lifetime):
        result = supervise(self.daemon_command, self.worker_command, self.ready_command, lifetime)
        with self.lock:
            self.result = "succeeded" if result else "failed"

    def join(self):
        if self.thread is not None:
            self.thread.join()


def stop(process):
    if process is not None:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def supervise(daemon_command, worker_command, ready_command, lifetime):
    if lifetime <= 0 or lifetime > 28800:
        raise ValueError("invalid execution lifetime")
    deadline = time.monotonic() + lifetime
    daemon = worker = readiness = None
    try:
        daemon = subprocess.Popen(daemon_command, start_new_session=True,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        while daemon.poll() is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            readiness = subprocess.Popen(ready_command, start_new_session=True,
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL)
            try:
                ready = readiness.wait(timeout=min(remaining, 1)) == 0
            except subprocess.TimeoutExpired:
                ready = False
            finally:
                stop(readiness)
                readiness = None
            if ready:
                break
            time.sleep(min(.025, max(0, deadline - time.monotonic())))
        else:
            return False
        if time.monotonic() >= deadline or daemon.poll() is not None:
            return False
        worker = subprocess.Popen(worker_command, start_new_session=True,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        while time.monotonic() < deadline and daemon.poll() is None:
            result = worker.poll()
            if result is not None:
                return result == 0
            time.sleep(min(.025, max(0, deadline - time.monotonic())))
        return False
    except OSError:
        return False
    finally:
        stop(readiness)
        stop(worker)
        stop(daemon)
