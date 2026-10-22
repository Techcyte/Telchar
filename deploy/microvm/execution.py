"""Image-local fresh Nix daemon and worker lifetime supervision."""
import argparse
import contextlib
import ctypes
import ipaddress
import json
import os
import select
import signal
import subprocess
import sys
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
        self.cancelled = threading.Event()

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

    def terminate(self):
        self.cancelled.set()

    def _execute(self, lifetime):
        result = supervise(self.daemon_command, self.worker_command, self.ready_command, lifetime, self.cancelled)
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


def supervise(daemon_command, worker_command, ready_command, lifetime, cancelled=None):
    if lifetime <= 0 or lifetime > 28800:
        raise ValueError("invalid execution lifetime")
    cancelled = cancelled if cancelled is not None else threading.Event()
    deadline = time.monotonic() + lifetime
    process = None
    try:
        specification = json.dumps([daemon_command, worker_command, ready_command, deadline])
        process = subprocess.Popen(
            [sys.executable, os.path.abspath(__file__), "--execution-init", specification],
            start_new_session=True, stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        while time.monotonic() < deadline and not cancelled.is_set():
            result = process.poll()
            if result is not None:
                return result == 0
            time.sleep(min(.025, max(0, deadline - time.monotonic())))
        return False
    except OSError:
        return False
    finally:
        if process is not None:
            if process.stdin is not None:
                process.stdin.close()
            with contextlib.suppress(ProcessLookupError):
                process.terminate()
            process.wait()


def enter_execution(specification):
    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM})
    os.unshare(os.CLONE_NEWPID | os.CLONE_NEWNS)
    libc = ctypes.CDLL(None, use_errno=True)
    mount = libc.mount
    mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_void_p]
    mount.restype = ctypes.c_int
    # Private propagation prevents execution mounts from reaching the runtime.
    if mount(None, b"/", None, (1 << 18) | (1 << 14), None) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))
    read_end, write_end = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(write_end)
        os.close(0)

        def watch_helper():
            try:
                os.read(read_end, 1)
            finally:
                os._exit(1)

        threading.Thread(target=watch_helper, daemon=True).start()
        # Procfs must describe the namespace entered by this fork.
        if mount(b"proc", b"/proc", b"proc", 2 | 4 | 8, None) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})
        return run_execution(*specification)

    os.close(read_end)

    def terminate(_signal, _frame):
        with contextlib.suppress(ProcessLookupError):
            os.kill(child, signal.SIGKILL)

    signal.signal(signal.SIGTERM, terminate)
    signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})
    try:
        while True:
            finished, status = os.waitpid(child, os.WNOHANG)
            if finished:
                return os.waitstatus_to_exitcode(status) == 0
            readable, _, _ = select.select([0], [], [], .025)
            if readable:
                terminate(None, None)
                os.waitpid(child, 0)
                return False
    finally:
        os.close(write_end)


def run_execution(daemon_command, worker_command, ready_command, deadline):
    if os.getpid() != 1:
        return False
    cancelled = threading.Event()
    daemon = worker = readiness = None
    try:
        daemon = subprocess.Popen(daemon_command, start_new_session=True,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        while daemon.poll() is None and not cancelled.is_set():
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
        if time.monotonic() >= deadline or daemon.poll() is not None or cancelled.is_set():
            return False
        worker = subprocess.Popen(worker_command, start_new_session=True,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        while time.monotonic() < deadline and daemon.poll() is None and not cancelled.is_set():
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


def main():
    from runtime import make_server

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8080)
    for name in ("daemon", "ready", "worker", "binding", "trust", "store"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    state = ExecutionState([args.daemon], [args.worker, args.binding, args.trust, args.store],
                           [args.ready], args.binding)
    def image_ready():
        try:
            commands = (args.daemon, args.ready, args.worker)
            parent = os.path.dirname(os.path.abspath(args.binding))
            return (all(os.path.isfile(path) and os.access(path, os.X_OK) for path in commands)
                    and os.path.isfile(args.trust) and os.access(args.trust, os.R_OK)
                    and os.path.getsize(args.trust) > 0
                    and os.path.isdir(parent) and os.access(parent, os.W_OK | os.X_OK)
                    and not os.path.lexists(args.binding))
        except OSError:
            return False

    server = make_server((args.listen, args.port), state, image_ready)

    def shutdown(_signal, _frame):
        state.terminate()
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    try:
        server.serve_forever()
    finally:
        state.terminate()
        state.join()
        server.server_close()


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--execution-init":
        try:
            result = enter_execution(json.loads(sys.argv[2]))
        except (ValueError, TypeError, OSError):
            result = False
        raise SystemExit(0 if result else 1)
    main()
