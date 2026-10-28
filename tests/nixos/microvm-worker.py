"""Start one real worker per simulated provider dispatch inside the test VM."""
import http.server
import json
import os
import subprocess
import sys
from pathlib import Path


class Worker(http.server.BaseHTTPRequestHandler):
    executions = {}

    def do_GET(self):
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
            sys.argv[1], str(binding), "/run/microvm-worker/trust.der",
            "unix:///nix/var/nix/daemon-socket/socket",
        ], stdout=sys.stdout, stderr=sys.stderr)
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()


http.server.HTTPServer(("192.168.1.3", 9877), Worker).serve_forever()
