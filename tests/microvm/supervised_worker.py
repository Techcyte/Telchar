"""Real Rust worker driven by production execution supervision."""
import importlib
import json
import os
import select
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "deploy/microvm"))
ExecutionState = importlib.import_module("execution").ExecutionState


def main():
    binary, binding, trust, builder = sys.argv[1:5]
    persistent = sys.argv[5:] == ["--persistent"]
    destination = Path(binding)
    initial_hook = None
    if not persistent:
        payload = json.loads(destination.read_text())
        identity = payload.pop("provider_id")
        payload["version"] = 1
        initial_hook = {"microvmId": identity, "runHookPayload": json.dumps(payload)}
        destination.unlink()
    with tempfile.TemporaryDirectory(prefix="telchar-supervised-worker-") as temporary:
        retained = Path(temporary) / "inputs"
        for name in ("NIX_STATE_DIR", "NIX_STORE_DIR", "NIX_LOG_DIR", "NIX_DAEMON_SOCKET_PATH"):
            os.environ.pop(name, None)
        os.environ["NIX_CONFIG"] = (
            "experimental-features = nix-command local-overlay-store read-only-local-store\n"
            "sandbox = true\nbuild-users-group =\nsubstituters =\n"
            "require-drop-supplementary-groups = false\n")
        nix = shutil.which("nix")
        assert nix is not None
        copied = subprocess.run([nix, "copy", "--to", f"local?root={retained}",
                                 "--no-check-sigs", str(Path(builder).parent.parent)],
                                capture_output=True)
        if copied.returncode:
            raise RuntimeError(copied.stderr.decode())
        socket = str(Path(temporary) / "import.sock")
        diagnostics = Path(temporary) / "daemon.log"
        launcher = Path(temporary) / "nix"
        launcher.write_text(f'#!/bin/sh\nexec {nix} -vvvvv "$@" 2>>{diagnostics}\n')
        launcher.chmod(0o700)
        os.environ.update({
            "NIX_DAEMON_SOCKET_PATH": socket,
            "TELCHAR_MICROVM_RETAINED_ROOT": str(retained),
            "TELCHAR_MICROVM_NIX": str(launcher),
            "TELCHAR_MICROVM_MAXIMUM_STORE_BYTES": str(2**40),
            "TELCHAR_MICROVM_MINIMUM_FREE_BYTES": "0",
        })
        os.environ["NIX_CONFIG"] += f"store = local?root={retained}\n"
        worker = Path(temporary) / "worker"
        worker.write_text(f'#!/bin/sh\nexec {binary} "$@" 2>>{diagnostics}\n')
        worker.chmod(0o700)
        state = ExecutionState([nix, "daemon"], [str(worker), binding, trust, "unix://" + socket],
                               [nix, "store", "ping", "--store", "unix://" + socket],
                               destination, Path(trust))
        private_jobs = set(Path(tempfile.gettempdir()).glob("telchar-job-*"))

        def completed():
            state.join()
            report = state.report()
            if not report["reset_complete"]:
                raise RuntimeError(f'{report}: {diagnostics.read_text()}')
            assert not destination.exists()
            private = list((retained / "nix/store").glob("*-dedicated-output"))
            assert not private
            with sqlite3.connect(retained / "nix/var/nix/db/db.sqlite") as database:
                assert not database.execute(
                    "SELECT path FROM ValidPaths WHERE path LIKE '%-dedicated-output'").fetchall()
            assert set(Path(tempfile.gettempdir()).glob("telchar-job-*")) == private_jobs
            report.update(helper_pid=os.getpid(), retained_root=str(retained),
                          binding_absent=True, private_outputs_absent=True,
                          private_jobs_absent=True)
            return report

        if persistent:
            # Fixture-only bounded provider hook channel; never a production listener.
            while True:
                if not select.select([sys.stdin], [], [], 60)[0]:
                    raise TimeoutError("supervised control deadline")
                line = sys.stdin.buffer.readline(32769)
                if not line or len(line) > 32768:
                    raise ValueError("invalid supervised control message")
                command = json.loads(line)
                if command["command"] == "start":
                    assert state.start_hook(command["hook"])
                    report = state.report()
                elif command["command"] == "join":
                    report = completed()
                elif command["command"] == "stop":
                    assert state.report()["reset_complete"]
                    break
                else:
                    raise ValueError("invalid supervised command")
                print(json.dumps(report), flush=True)
        else:
            assert initial_hook is not None
            assert state.start_hook(initial_hook)
            completed()
        for directory, _, _ in os.walk(retained):
            os.chmod(directory, 0o700)
    return 0


if __name__ == "__main__":
    sys.exit(main())
