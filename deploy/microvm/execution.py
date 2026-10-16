"""Image-local fresh Nix daemon and worker lifetime supervision."""
import contextlib
import os
import signal
import subprocess
import time


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
