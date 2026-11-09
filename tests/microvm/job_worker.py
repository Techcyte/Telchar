"""Real WSS worker handoff fixture; canonical-store sandboxing stays disabled."""
import importlib
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "deploy/microvm"))
retained_store = importlib.import_module("retained_store")
job_daemon = retained_store.job_daemon
mount = retained_store.mount
unmount = retained_store.unmount
wait_prepared = retained_store.wait_prepared


def main():
    with tempfile.TemporaryDirectory(prefix="telchar-worker-job-") as temporary:
        directory = Path(temporary)
        environment = os.environ.copy()
        environment["NIX_CONFIG"] += "\nexperimental-features = nix-command local-overlay-store read-only-local-store\n"
        environment["TELCHAR_MICROVM_JOB_SOCKET"] = str(directory / "prepare.sock")
        retained = Path(environment.get("MICROVM_TEST_RETAINED_ROOT", directory / "inputs"))
        database = retained / "nix/var/nix/db/db.sqlite"
        if database.exists():
            registration = subprocess.run(["nix-store", "--store", f"local?root={retained}", "--dump-db"],
                                          env=environment, capture_output=True)
            if registration.returncode:
                raise RuntimeError(registration.stderr.decode())
            loaded = subprocess.run(["nix-store", "--store", "local", "--load-db"],
                                    input=registration.stdout, env=environment, capture_output=True)
            if loaded.returncode:
                raise RuntimeError(loaded.stderr.decode())
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(environment["TELCHAR_MICROVM_JOB_SOCKET"])
            listener.listen(1)
            listener.settimeout(.05)
            worker = subprocess.Popen(sys.argv[1:], env=environment)
            try:
                deadline = time.monotonic() + 60
                while True:
                    status = worker.poll()
                    if status is not None:
                        return status
                    if time.monotonic() >= deadline:
                        raise TimeoutError("worker preparation deadline")
                    try:
                        channel, _ = listener.accept()
                        break
                    except socket.timeout:
                        continue
                with channel:
                    wait_prepared(channel, deadline)
                    prepared_at = time.monotonic()
                    print("job fixture: worker prepared; initializing private store", file=sys.stderr, flush=True)
                    with sqlite3.connect(str(Path(os.environ["NIX_STATE_DIR"]) / "db/db.sqlite")) as database:
                        paths = [row[0] for row in database.execute("SELECT path FROM ValidPaths")]
                    daemon_environment = environment.copy()
                    for name in ("NIX_STATE_DIR", "NIX_STORE_DIR", "NIX_LOG_DIR", "NIX_DAEMON_SOCKET_PATH"):
                        daemon_environment.pop(name, None)
                    daemon_environment["NIX_CONFIG"] += (
                        "\nexperimental-features = nix-command local-overlay-store read-only-local-store\nrequire-drop-supplementary-groups = false\n")
                    shell = Path('/bin/sh').resolve().parent.parent
                    source = environment.get("TELCHAR_TEST_SOURCE_STORE", "auto")
                    copied_shell = subprocess.run(
                        ['nix', 'copy', '--from', source, '--to', f'local?root={retained}', '--no-check-sigs', str(shell)],
                        env=daemon_environment, capture_output=True)
                    if copied_shell.returncode:
                        raise RuntimeError(copied_shell.stderr.decode())
                    copied = subprocess.run(
                        ["nix", "copy", "--from", sys.argv[-1], "--to", f"local?root={retained}",
                         "--no-check-sigs", *paths], env=daemon_environment, capture_output=True)
                    if copied.returncode:
                        raise RuntimeError(copied.stderr.decode())
                    daemon = directory / 'nix'
                    nix = shutil.which('nix')
                    assert nix is not None
                    daemon.write_text('#!/bin/sh\nexec ' + nix + ' -vvvvv "$@"\n')
                    daemon.chmod(0o700)
                    with job_daemon(retained, directory / "job", daemon,
                                    daemon_environment, deadline) as endpoint:
                        mounted = []
                        try:
                            for name in ("bin", "usr", "lib", "lib64"):
                                source = Path("/") / name
                                if source.exists():
                                    target = directory / "job/merged" / name
                                    target.mkdir()
                                    mount(source, target, flags=4096)
                                    mounted.append(target)
                            checked = subprocess.run(["nix", "path-info", "--store", endpoint, *paths],
                                                     env=daemon_environment, capture_output=True)
                            if checked.returncode:
                                raise RuntimeError(checked.stderr.decode())
                            print(f"job fixture: private store ready after {time.monotonic() - prepared_at:.3f}s; worker_status={worker.poll()}", file=sys.stderr, flush=True)
                            channel.sendall(endpoint.encode() + b"\n")
                            status = worker.wait(timeout=max(.001, deadline - time.monotonic()))
                            return status
                        finally:
                            if worker.poll() is None:
                                worker.kill()
                            worker.wait()
                            for target in reversed(mounted):
                                unmount(target)
            finally:
                if worker.poll() is None:
                    worker.kill()
                worker.wait()


if __name__ == "__main__":
    raise SystemExit(main())
