"""Loopback provider simulator for NixOS MicroVM startup, not AWS qualification."""
import http.client
import http.server
import json


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def respond(self, content, content_type):
        body = content.encode()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path != "/":
            assert self.path.startswith("/2025-09-09/microvms")
            payload = json.loads(json.loads(body)["runHookPayload"])
            payload.pop("version")
            payload["provider_id"] = "microvm-build"
            worker = http.client.HTTPConnection("worker", 9877, timeout=2)
            worker.request("POST", "/run", json.dumps(payload))
            assert worker.getresponse().status == 202
            worker.close()
            self.respond(json.dumps({
                "microvmId": "microvm-build", "state": "PENDING",
                "endpoint": "https://unused.invalid", "imageArn": "fixture",
                "imageVersion": "1", "maximumDurationInSeconds": 60, "startedAt": 1,
            }), "application/json")
            return
        assert b"Action=GetCallerIdentity" in body
        assert "Credential=fixture-key/" in self.headers["Authorization"]
        self.respond(
            '<GetCallerIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">'
            '<GetCallerIdentityResult><Account>116392764028</Account>'
            '<Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId>'
            '</GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId>'
            '</ResponseMetadata></GetCallerIdentityResponse>',
            "text/xml",
        )

    def do_GET(self):
        assert self.path.startswith("/2025-09-09/microvm-images/")
        assert self.path.endswith("/versions/1")
        assert "Credential=fixture-key/" in self.headers["Authorization"]
        self.respond(json.dumps({
            "imageArn": "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder",
            "imageVersion": "1", "state": "SUCCESSFUL", "status": "ACTIVE",
            "baseImageArn": "arn:aws:lambda:us-west-2:aws:microvm-image:al2023-1",
            "buildRoleArn": "arn:aws:iam::116392764028:role/fixture", "createdAt": 1,
            "cpuConfigurations": [{"architecture": "ARM_64"}],
            "resources": [{"minimumMemoryInMiB": 2048}],
        }), "application/json")


http.server.HTTPServer(("127.0.0.1", 9876), Provider).serve_forever()
