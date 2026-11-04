"""Real WSS worker handoff fixture; canonical-store sandboxing stays disabled."""
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "deploy/microvm"))
from retained_store import job_daemon, mount, unmount, wait_prepared


def main():
    with tempfile.TemporaryDirectory(prefix="telchar-worker-job-") as temporary:
        directory = Path(temporary)
        environment = os.environ.copy()
        environment["TELCHAR_MICROVM_JOB_SOCKET"] = str(directory / "prepare.sock")
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
                    retained = directory / "inputs"
                    with sqlite3.connect(str(Path(os.environ["NIX_STATE_DIR"]) / "db/db.sqlite")) as database:
                        paths = [row[0] for row in database.execute("SELECT path FROM ValidPaths")]
                    daemon_environment = environment.copy()
                    for name in ("NIX_STATE_DIR", "NIX_STORE_DIR", "NIX_LOG_DIR", "NIX_DAEMON_SOCKET_PATH"):
                        daemon_environment.pop(name, None)
                    daemon_environment["NIX_CONFIG"] += (
                        "\nexperimental-features = nix-command local-overlay-store read-only-local-store\nrequire-drop-supplementary-groups = false\n")
                    shell = Path('/bin/sh').resolve().parent.parent
                    copied_shell = subprocess.run(
                        ['nix', 'copy', '--to', f'local?root={retained}', '--no-check-sigs', str(shell)],
                        env=daemon_environment, capture_output=True)
                    if copied_shell.returncode:
                        raise RuntimeError(copied_shell.stderr.decode())
                    copied = subprocess.run(
                        ["nix", "copy", "--from", sys.argv[-1], "--to", f"local?root={retained}",
                         "--no-check-sigs", *paths], env=daemon_environment, capture_output=True)
                    if copied.returncode:
                        raise RuntimeError(copied.stderr.decode())
                    daemon = directory / 'nix'
                    daemon.write_text('#!/bin/sh\nexec ' + shutil.which('nix') + ' -vvvvv "$@"\n')
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
