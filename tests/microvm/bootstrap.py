"""Write one runtime-only execution binding for the disposable worker fixture."""
import importlib.util
import json
import os
import threading
from pathlib import Path
from urllib.parse import urlsplit


def load_runtime():
    path = Path(__file__).resolve().parents[2] / "deploy/microvm/runtime.py"
    spec = importlib.util.spec_from_file_location("microvm_runtime", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runtime = load_runtime()


class ExecutionState(runtime.ProbeState):
    def __init__(self, command, destination):
        super().__init__(command)
        self.destination = destination
        self.hook_lock = threading.Lock()

    def start_hook(self, payload):
        with self.hook_lock:
            if self.identity is not None:
                return self.identity == payload.get("microvmId")
            write_bootstrap(payload, self.destination)
            return self.start(payload["microvmId"])


def write_bootstrap(hook, destination):
    identity = hook.get("microvmId")
    if not isinstance(identity, str) or not identity or len(identity) > 256:
        raise ValueError("invalid instance identity")
    payload = hook.get("runHookPayload")
    if not isinstance(payload, str) or len(payload.encode()) > 4096:
        raise ValueError("invalid bootstrap payload")
    value = json.loads(payload)
    if not isinstance(value, dict) or set(value) != {"endpoint", "credential", "certificate", "execution"}:
        raise ValueError("invalid execution fields")
    if any(not isinstance(item, str) or not item for item in value.values()):
        raise ValueError("invalid execution values")
    endpoint = urlsplit(value["endpoint"])
    if endpoint.scheme != "wss" or not endpoint.hostname or endpoint.username or endpoint.password or endpoint.fragment:
        raise ValueError("invalid execution endpoint")
    if len(value["credential"]) != 64:
        raise ValueError("invalid execution credential")
    value["microvmId"] = identity
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        json.dump(value, output)


def main():
    import argparse
    import subprocess
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", required=True)
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--listen", default="127.0.0.1")
    args = parser.parse_args()
    destination = Path("/run/execution.json")
    state = ExecutionState([args.worker, str(destination)], destination)

    def ready():
        result = subprocess.run(["nix", "--offline", "store", "ping", "--store", "daemon"],
                                capture_output=True, timeout=5)
        return result.returncode == 0

    runtime.make_server((args.listen, args.port), state, ready).serve_forever()


if __name__ == "__main__":
    main()
