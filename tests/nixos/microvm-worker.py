"""Start one real worker per simulated provider dispatch inside the test VM."""
import http.server
import json
import os
import subprocess
import sys
from pathlib import Path


class Worker(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_POST(self):
        assert self.path == "/run"
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        binding = Path("/run/microvm-worker/execution.json")
        with binding.open("x") as output:
            os.chmod(binding, 0o600)
            json.dump(payload, output)
        subprocess.Popen([
            sys.argv[1], str(binding), "/run/microvm-worker/trust.der",
            "unix:///nix/var/nix/daemon-socket/socket",
        ], stdout=sys.stdout, stderr=sys.stderr)
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()


http.server.HTTPServer(("192.168.1.3", 9877), Worker).serve_forever()
