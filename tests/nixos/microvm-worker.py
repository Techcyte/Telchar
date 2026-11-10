"""Start one real worker per simulated provider dispatch inside the test VM."""
import http.server
import importlib
import json
import os
import shlex
import sqlite3
import subprocess
import sys
from pathlib import Path


def execute(binary, nix, binding, *runtime_paths):
    supervise = importlib.import_module("execution").supervise

    directory = Path(binding).parent
    retained = directory / "inputs"
    endpoint = "unix://" + str(directory / "import.sock")
    os.environ["NIX_CONFIG"] = (
        os.environ.get("NIX_CONFIG", "")
        + "\nexperimental-features = nix-command local-overlay-store read-only-local-store\n")
    # The execution image registers its runtime closure before accepting work.
    # Seed only that closure; fresh text inputs must still cross the gateway.
    subprocess.run([
        nix, "copy", "--from", "daemon", "--to", f"local?root={retained}",
        "--no-check-sigs", *runtime_paths,
    ], check=True)
    os.environ.update({
        "NIX_DAEMON_SOCKET_PATH": str(directory / "import.sock"),
        "TELCHAR_MICROVM_RETAINED_ROOT": str(retained),
        "TELCHAR_MICROVM_NIX": nix,
        "TELCHAR_MICROVM_MAXIMUM_STORE_BYTES": str(2**30),
        "TELCHAR_MICROVM_MINIMUM_FREE_BYTES": "0",
        "TELCHAR_MICROVM_DIAGNOSTICS": "1",
    })
    os.environ["NIX_CONFIG"] += f"store = local?root={retained}\n"
    payload = json.loads(Path(binding).read_text())
    worker = directory / "worker"
    worker.write_text(
        "#!/bin/sh\nexec " + shlex.quote(binary) + ' "$@" 2>>'
        + shlex.quote(str(directory / "worker.log")) + "\n")
    worker.chmod(0o700)
    succeeded = supervise(
        [nix, "daemon"],
        [str(worker), binding, "/run/microvm-worker/trust.der", endpoint],
        [nix, "store", "ping", "--store", endpoint],
        payload["lifetime_seconds"],
    )
    assert not list(retained.glob("telchar-job-*")), "private job directories survive teardown"
    with sqlite3.connect(retained / "nix/var/nix/db/db.sqlite") as database:
        assert not database.execute(
            "SELECT path FROM ValidPaths WHERE path LIKE '%-alice-%' OR path LIKE '%-bob-%' OR path LIKE '%-microvm-service-output'"
        ).fetchall(), "private build outputs retained"
    assert not list((retained / "nix/store").glob("*-alice-*"))
    assert not list((retained / "nix/store").glob("*-bob-*"))
    assert not list((retained / "nix/store").glob("*-microvm-service-output"))
    if not succeeded:
        print((directory / "worker.log").read_text(), flush=True)
    return 0 if succeeded else 1


class Worker(http.server.BaseHTTPRequestHandler):
    executions = {}

    def do_GET(self):
        if self.path == "/statuses":
            body = json.dumps({key: process.poll() for key, process in Worker.executions.items()}).encode()
        else:
            provider_id = self.path.removeprefix("/executions/")
            process = Worker.executions[provider_id]
            body = json.dumps({"terminated": process.poll() is not None}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass

    def do_POST(self):
        assert self.path == "/run"
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        directory = Path("/run/microvm-worker") / payload["provider_id"]
        directory.mkdir(mode=0o700)
        binding = directory / "execution.json"
        with binding.open("x") as output:
            os.chmod(binding, 0o600)
            json.dump(payload, output)
        Worker.executions[payload["provider_id"]] = subprocess.Popen([
            sys.executable, __file__, "execute", sys.argv[1], sys.argv[2], str(binding),
            *sys.argv[3:],
        ], stdout=sys.stdout, stderr=sys.stderr)
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()


if __name__ == "__main__":
    if sys.argv[1] == "execute":
        sys.exit(execute(*sys.argv[2:]))
    http.server.HTTPServer(("192.168.1.3", 9877), Worker).serve_forever()
