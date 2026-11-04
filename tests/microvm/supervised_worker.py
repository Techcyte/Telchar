"""Real Rust worker driven by production execution supervision."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "deploy/microvm"))
from execution import ExecutionState


def main():
    binary, binding, trust, builder = sys.argv[1:]
    destination = Path(binding)
    payload = json.loads(destination.read_text())
    identity = payload.pop("provider_id")
    payload["version"] = 1
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
        assert state.start_hook({"microvmId": identity, "runHookPayload": json.dumps(payload)})
        state.join()
        if not state.report()["reset_complete"]:
            raise RuntimeError(f'{state.report()}: {diagnostics.read_text()}')
        assert not destination.exists()
        assert not list((retained / "nix/store").glob("*-dedicated-output"))
        for directory, _, _ in os.walk(retained):
            os.chmod(directory, 0o700)
    return 0


if __name__ == "__main__":
    sys.exit(main())
