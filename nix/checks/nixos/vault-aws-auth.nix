# Verifies Vault AWS authentication through an EC2 instance profile for renewal candidates.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
let
  identityServices = pkgs.writeText "identity-services.py" ''
    import base64
    import json
    import os
    import subprocess
    import tempfile
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from threading import Thread

    observed = "/tmp/observed"
    os.makedirs(observed, exist_ok=True)

    class MetadataHandler(BaseHTTPRequestHandler):
        def do_PUT(self):
            if self.path != "/latest/api/token":
                self.send_error(404)
                return
            open(observed + "/imds-token", "w").write("requested")
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"metadata-token")

        def do_GET(self):
            if self.headers.get("X-aws-ec2-metadata-token") != "metadata-token":
                self.send_error(401)
                return
            if self.path == "/latest/meta-data/iam/security-credentials/":
                open(observed + "/imds-role", "w").write("requested")
                body = b"telchar-gateway-role"
            elif self.path == "/latest/meta-data/iam/security-credentials/telchar-gateway-role":
                open(observed + "/imds-credentials", "w").write("requested")
                body = json.dumps({
                    "Code": "Success",
                    "AccessKeyId": "AKIAINSTANCEPROFILE",
                    "SecretAccessKey": "instance-profile-secret",
                    "Token": "instance-profile-session",
                    "Expiration": "2035-01-01T00:00:00Z",
                }).encode()
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format, *args):
            pass

    class VaultHandler(BaseHTTPRequestHandler):
        def reply(self, value):
            body = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def request_json(self):
            length = int(self.headers.get("Content-Length", "0"))
            return json.loads(self.rfile.read(length) or b"{}")

        def do_POST(self):
            request = self.request_json()
            if self.path == "/v1/auth/aws/login":
                open(observed + "/vault-login", "w").write(json.dumps(request))
                assert request["role"] == "telchar-gateway"
                assert request["iam_http_request_method"] == "POST"
                assert "iam_request_url" in request
                assert "iam_request_body" in request
                assert "iam_request_headers" in request
                self.reply({"auth": {"client_token": "vault-session-token", "lease_duration": 300}})
                return
            if self.path == "/v1/ssh/sign/telchar-host":
                assert self.headers.get("X-Vault-Token") == "vault-session-token"
                public_key = request["public_key"]
                with tempfile.TemporaryDirectory() as directory:
                    public_key_path = directory + "/host.pub"
                    open(public_key_path, "w").write(public_key)
                    subprocess.run([
                        "${pkgs.openssh}/bin/ssh-keygen", "-q", "-s", "/var/lib/vault-fixture/host-ca",
                        "-I", "vault-renewed", "-h", "-n", "telchar.services.hub.techcyte.com",
                        "-V", "-1m:+10m", public_key_path,
                    ], check=True)
                    signed_key = open(directory + "/host-cert.pub").read()
                open(observed + "/ssh-sign", "w").write("requested")
                self.reply({"data": {"signed_key": signed_key}})
                return
            self.send_error(404)

        def do_GET(self):
            if self.path != "/v1/kv/data/telchar/nomad":
                self.send_error(404)
                return
            assert self.headers.get("X-Vault-Token") == "vault-session-token"
            open(observed + "/nomad-secret", "w").write("requested")
            self.reply({"data": {"data": {"token": "vault-nomad-token"}}})

        def log_message(self, format, *args):
            pass

    Thread(target=lambda: ThreadingHTTPServer(("0.0.0.0", 80), MetadataHandler).serve_forever(), daemon=True).start()
    ThreadingHTTPServer(("0.0.0.0", 8200), VaultHandler).serve_forever()
  '';
in
pkgs.testers.nixosTest {
  name = "telchar-nixos-vault-aws-auth";

  nodes = {
    identity =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        environment.systemPackages = [
          pkgs.openssh
          pkgs.python3
        ];
        system.stateVersion = "26.05";
      };

    gateway =
      { ... }:
      {
        imports = [ standaloneModule ];

        services.telchar = {
          package = telchar;
          vaultAwsAuth = {
            enable = true;
            address = "http://identity:8200";
            role = "telchar-gateway";
            authMount = "aws";
            sshSignPath = "ssh/sign/telchar-host";
            nomadSecretPath = "kv/data/telchar/nomad";
            metadataEndpoint = "http://identity";
            renewal = {
              enable = true;
              interval = "15min";
              randomizedDelaySec = "5min";
            };
          };
          ingress.openssh = {
            hostKeyFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key";
            hostCertificateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub";
            trustedUserCAKeysFile = "/var/lib/telchar/ssh/client-ca.pub";
            authorizedPrincipalsFile = "/var/lib/telchar/ssh/authorized_principals";
          };
          sshHostCertificateRenewal = {
            enable = true;
            candidateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub";
          };
          nomad = {
            tokenFile = "/var/lib/telchar/credentials/nomad-token";
            renewal = {
              enable = true;
              candidateFile = "/var/lib/telchar/credentials/nomad-token.candidate";
            };
          };
          settings.backends.local = {
            name = "local";
            inherit system;
            maximum_concurrent_builds = 1;
          };
        };

        system.stateVersion = "26.05";
      };
  };

  testScript = ''
    identity.start()
    identity.succeed("install -d -m 700 /var/lib/vault-fixture && ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/vault-fixture/host-ca")
    identity.succeed("systemd-run --unit=identity-services ${pkgs.python3}/bin/python3 ${identityServices}")
    identity.wait_until_succeeds("ss -ltn | grep -q ':80 ' && ss -ltn | grep -q ':8200 '")

    gateway.start()
    gateway.succeed("systemctl stop telchar.service telchar-sshd.service || true")
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/ssh /var/lib/telchar/credentials")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/ssh_host_ed25519_key")
    gateway.succeed("chown -R telchar:telchar /var/lib/telchar/ssh /var/lib/telchar/credentials")
    original_key = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key").split()[0]

    gateway.succeed("systemctl start telchar-credential-renewal.service")
    identity.succeed("test -f /tmp/observed/imds-token")
    identity.succeed("test -f /tmp/observed/imds-role")
    identity.succeed("test -f /tmp/observed/imds-credentials")
    identity.succeed("test -f /tmp/observed/vault-login")
    identity.succeed("test -f /tmp/observed/ssh-sign")
    identity.succeed("test -f /tmp/observed/nomad-secret")
    identity.fail("test -e /tmp/observed/approle-login")

    gateway.succeed("ssh-keygen -L -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub | grep -q vault-renewed")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub) = 400")
    gateway.succeed("test $(stat -c %U:%G /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub) = telchar:telchar")
    gateway.succeed("test $(cat /var/lib/telchar/credentials/nomad-token.candidate) = vault-nomad-token")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/nomad-token.candidate) = 400")
    gateway.succeed("test $(stat -c %U:%G /var/lib/telchar/credentials/nomad-token.candidate) = telchar:telchar")
    gateway.succeed("ssh-keygen -L -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | grep -q vault-renewed")
    gateway.succeed("test $(cat /var/lib/telchar/credentials/nomad-token) = vault-nomad-token")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/nomad-token) = 400")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key | cut -d' ' -f1) = " + original_key)
    gateway.succeed("systemctl is-active telchar-credential-renewal.timer")
    gateway.succeed("systemctl list-timers --all telchar-credential-renewal.timer | grep -q telchar-credential-renewal.timer")
    gateway.succeed("systemctl show telchar-ssh-host-certificate-renewal.service -p After --value | grep -q telchar-vault-aws-auth.service")
    gateway.succeed("systemctl show telchar-nomad-credential-renewal.service -p After --value | grep -q telchar-vault-aws-auth.service")
    gateway.fail("systemctl cat telchar-credential-renewal.service | grep -E 'systemctl (start|restart)'")
    gateway.fail("find /var/lib/telchar -iname '*role_id*' -o -iname '*secret_id*' | grep .")
    gateway.fail("systemctl cat telchar-vault-aws-auth.service | grep -E 'role_id|secret_id|approle|AKIAINSTANCEPROFILE|instance-profile-secret|instance-profile-session|vault-session-token'")
  '';
}
