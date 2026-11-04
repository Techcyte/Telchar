"""Private job store over an immutable retained local input generation."""
import contextlib
import ctypes
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time
from urllib.parse import quote


def check_capacity(retained, maximum_bytes, minimum_free_bytes, deadline):
    if maximum_bytes <= 0 or minimum_free_bytes < 0:
        raise ValueError("invalid retained store bounds")
    if time.monotonic() >= deadline:
        raise TimeoutError("retained store scan expired")
    retained = Path(retained)
    if shutil.disk_usage(retained).free < minimum_free_bytes:
        raise OSError("retained store capacity exceeded")
    allocated = 0
    pending = [retained / "nix/store"]
    while pending:
        if time.monotonic() >= deadline:
            raise TimeoutError("retained store scan expired")
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                if time.monotonic() >= deadline:
                    raise TimeoutError("retained store scan expired")
                allocated += entry.stat(follow_symlinks=False).st_blocks * 512
                if allocated > maximum_bytes:
                    raise OSError("retained store capacity exceeded")
                if entry.is_dir(follow_symlinks=False):
                    pending.append(entry.path)


def mount(source, target, flags=0, filesystem=None, options=None):
    libc = ctypes.CDLL(None, use_errno=True)
    operation = libc.mount
    operation.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
                          ctypes.c_ulong, ctypes.c_void_p]
    result = operation(os.fsencode(source) if source is not None else None,
                       os.fsencode(target), os.fsencode(filesystem) if filesystem else None,
                       flags, os.fsencode(options) if options else None)
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(target))


def unmount(target):
    libc = ctypes.CDLL(None, use_errno=True)
    operation = libc.umount2
    operation.argtypes = [ctypes.c_char_p, ctypes.c_int]
    if operation(os.fsencode(target), 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(target))


@contextlib.contextmanager
def job_store(retained, directory):
    retained = Path(retained).resolve(strict=True)
    directory = Path(directory)
    directory.mkdir(mode=0o700)
    mounted = []
    try:
        lower = directory / "lower"
        lower.mkdir()
        for name in ("nix/store", "nix/var/nix"):
            target = lower / name
            target.mkdir(parents=True)
            mount(retained / name, target, flags=4096)
            mounted.append(target)
            mount(None, target, flags=4096 | 32 | 1)
        upper = directory / "upper"
        work = directory / "work"
        merged = directory / "merged"
        for path in (upper, work, merged / "nix/store"):
            path.mkdir(parents=True)
        mount("overlay", merged / "nix/store", filesystem="overlay",
              options=f"lowerdir={lower}/nix/store,upperdir={upper},workdir={work}")
        mounted.append(merged / "nix/store")
        lower_store = quote(f"local?root={lower}&read-only=true", safe="")
        yield f"local-overlay://?root={merged}&lower-store={lower_store}&upper-layer={upper}"
    finally:
        # Preserve failed teardown state rather than deleting through an active mount.
        for path in reversed(mounted):
            unmount(path)
        shutil.rmtree(directory)


def wait_prepared(channel, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("job preparation expired")
    channel.settimeout(remaining)
    prepared = bytearray()
    while len(prepared) < 9:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("job preparation expired")
        channel.settimeout(remaining)
        chunk = channel.recv(9 - len(prepared))
        if not chunk:
            raise RuntimeError("worker disconnected during preparation")
        prepared.extend(chunk)
    if prepared != b"prepared\n":
        raise ValueError("invalid job preparation")


@contextlib.contextmanager
def prepared_job(channel, retained, directory, nix, environment, deadline):
    wait_prepared(channel, deadline)
    with job_daemon(retained, directory, nix, environment, deadline) as store:
        channel.sendall(store.encode() + b"\n")
        yield store


@contextlib.contextmanager
def job_daemon(retained, directory, nix, environment, deadline):
    directory = Path(directory)
    process = None
    if time.monotonic() >= deadline:
        raise TimeoutError("job Nix daemon readiness expired")
    with job_store(retained, directory) as store:
        socket = directory / "daemon.sock"
        daemon_environment = environment.copy()
        daemon_environment["NIX_DAEMON_SOCKET_PATH"] = str(socket)
        daemon_environment["NIX_CONFIG"] = (
            environment.get("NIX_CONFIG", "") + "\nstore = " + store + "\n")
        with (directory / "daemon.log").open("wb") as diagnostics:
            try:
                process = subprocess.Popen(
                    [str(nix), "daemon"], env=daemon_environment,
                    start_new_session=True, stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=diagnostics)
                while not socket.exists():
                    if process.poll() is not None:
                        raise RuntimeError("job Nix daemon exited before readiness")
                    if time.monotonic() >= deadline:
                        raise TimeoutError("job Nix daemon readiness expired")
                    time.sleep(.01)
                yield "unix://" + str(socket)
                if process.poll() is not None:
                    raise RuntimeError("job Nix daemon exited during execution")
            finally:
                if process is not None:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
