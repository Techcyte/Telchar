# Verifies Vault AWS authentication through an EC2 instance profile for renewal candidates.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
let
  vault =
    (import pkgs.path {
      inherit system;
      config.allowUnfreePredicate = package: pkgs.lib.getName package == "vault-bin";
    }).vault-bin;
  identityServices = pkgs.writeText "identity-services.py" ''
    import base64
    import json
    import os
    import subprocess
    import tempfile
    import time
    import urllib.error
    import urllib.request
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from threading import Thread

    observed = "/tmp/observed"
    os.makedirs(observed, exist_ok=True)

    metadata_token_requests = 0

    def delay_success():
        if os.path.exists(observed + "/delay-success"):
            time.sleep(1.6)

    class MetadataHandler(BaseHTTPRequestHandler):
        def do_PUT(self):
            if self.path != "/latest/api/token":
                self.send_error(404)
                return
            global metadata_token_requests
            metadata_token_requests += 1
            open(observed + "/imds-token", "w").write(str(metadata_token_requests))
            if os.path.exists(observed + "/stall-imds"):
                time.sleep(30)
                return
            if os.path.exists(observed + "/oversized-imds"):
                self.send_response(200)
                self.send_header("Content-Length", str(2 * 1024 * 1024))
                self.end_headers()
                return
            if metadata_token_requests == 1:
                self.send_error(503)
                return
            delay_success()
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
            delay_success()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format, *args):
            pass

    class VaultHandler(BaseHTTPRequestHandler):
        def reply(self, value):
            body = json.dumps(value).encode()
            delay_success()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def forward_vault(self, method, path, payload=None):
            token = open("/run/vault-test-token").read().strip()
            request = urllib.request.Request(
                "http://127.0.0.1:8201/v1/" + path,
                data=None if payload is None else json.dumps(payload).encode(),
                method=method,
                headers={"X-Vault-Token": token, "Content-Type": "application/json"},
            )
            try:
                response = urllib.request.urlopen(request, timeout=2)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                body = response.read()
                self.send_response(response.code)
                self.send_header("Content-Type", response.headers.get("Content-Type", "text/plain"))
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
                open(observed + "/ssh-sign", "w").write("requested")
                self.forward_vault("POST", "ssh-host-signer/sign/telchar-host", request)
                return
            self.send_error(404)

        def do_GET(self):
            assert self.headers.get("X-Vault-Token") == "vault-session-token"
            if self.path == "/v1/kv/data/telchar/nomad":
                open(observed + "/nomad-secret", "w").write("requested")
                self.reply({"data": {"secret_id": "vault-nomad-token"}})
                return
            if self.path == "/v1/ssh-host-signer/public_key":
                self.forward_vault("GET", "ssh-host-signer/public_key")
                return
            if self.path == "/v1/ssh-client-signer/public_key":
                self.forward_vault("GET", "ssh-client-signer/public_key")
                return
            self.send_error(404)

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
        imports = [
          standaloneModule
          ../../nixos-vault-aws.nix
        ];

        services.telchar = {
          package = telchar;
          vaultAwsAuth = {
            enable = true;
            address = "http://identity:8200";
            role = "telchar-gateway";
            authMount = "aws";
            sshSignPath = "ssh/sign/telchar-host";
            hostCAPath = "ssh-host-signer/public_key";
            clientCAPath = "ssh-client-signer/public_key";
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
            expectedSigningCAFile = "/var/lib/telchar/ssh/host-ca.pub";
            expectedPrincipals = [ "gateway.test" ];
            minimumRemainingValiditySec = 300;
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
    identity.succeed("install -d -m 700 /var/lib/vault-fixture; umask 077; head -c 32 /dev/urandom | base64 > /run/vault-test-token")
    identity.succeed("systemd-run --unit=vault-test --setenv=PATH=/run/current-system/sw/bin ${pkgs.runtimeShell} -c 'export VAULT_DEV_ROOT_TOKEN_ID=$(cat /run/vault-test-token); exec ${vault}/bin/vault server -dev -dev-listen-address=127.0.0.1:8201 >/run/vault-test.log 2>&1'")
    try:
        identity.wait_until_succeeds("${pkgs.curl}/bin/curl -fsS http://127.0.0.1:8201/v1/sys/health >/dev/null 2>&1", timeout=30)
    except Exception:
        print(identity.succeed("grep -iE 'error|failed|permission|cannot' /run/vault-test.log || true"))
        raise
    vault_env = "export VAULT_ADDR=http://127.0.0.1:8201 VAULT_TOKEN=$(cat /run/vault-test-token); "
    for mount in ["ssh-host-signer", "ssh-client-signer"]:
        identity.succeed(vault_env + f"${vault}/bin/vault secrets enable -path={mount} ssh >/dev/null; ${vault}/bin/vault write {mount}/config/ca generate_signing_key=true >/dev/null; ${vault}/bin/vault read -field=public_key {mount}/config/ca > /var/lib/vault-fixture/{mount}.pub")
    identity.succeed(vault_env + "${vault}/bin/vault write ssh-host-signer/roles/telchar-host key_type=ca allow_host_certificates=true allow_user_certificates=false allowed_domains=gateway.test allow_bare_domains=true ttl=600 max_ttl=600 >/dev/null")
    identity.succeed("systemd-run --unit=identity-services ${pkgs.python3}/bin/python3 ${identityServices}")
    identity.wait_until_succeeds("ss -ltn | grep -q ':80 ' && ss -ltn | grep -q ':8200 '")

    gateway.start()
    gateway.succeed("systemctl stop telchar.service telchar-sshd.service || true")
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/ssh /var/lib/telchar/credentials")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/ssh_host_ed25519_key")
    host_ca = identity.succeed("cat /var/lib/vault-fixture/ssh-host-signer.pub").strip()
    host_ca_hash = identity.succeed("sha256sum /var/lib/vault-fixture/ssh-host-signer.pub").split()[0]
    gateway.succeed("printf '%s\\n' '" + host_ca + "' > /var/lib/telchar/ssh/host-ca.pub")
    gateway.succeed("chown -R telchar:telchar /var/lib/telchar/ssh /var/lib/telchar/credentials")
    original_key = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key").split()[0]
    gateway.succeed("printf root-vault-victim > /root/vault-victim && ln -s /root/vault-victim /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub.tmp && ln -s /root/vault-victim /var/lib/telchar/credentials/nomad-token.candidate.tmp")
    identity.succeed("touch /tmp/observed/delay-success")

    gateway.succeed("systemctl start telchar-credential-renewal.service")
    identity.succeed("rm /tmp/observed/delay-success")
    gateway.succeed("test $(cat /root/vault-victim) = root-vault-victim")
    gateway.succeed("test $(stat -c %U:%G /root/vault-victim) = root:root")
    gateway.succeed("test $(systemctl show telchar-vault-aws-auth.service -p User --value) = telchar")
    gateway.succeed("test $(systemctl show telchar-ssh-host-certificate-renewal.service -p User --value) = telchar")
    gateway.succeed("test $(systemctl show telchar-nomad-credential-renewal.service -p User --value) = telchar")
    identity.succeed("test $(cat /tmp/observed/imds-token) -ge 2")
    identity.succeed("test -f /tmp/observed/imds-role")
    identity.succeed("test -f /tmp/observed/imds-credentials")
    identity.succeed("test -f /tmp/observed/vault-login")
    identity.succeed("test -f /tmp/observed/ssh-sign")
    identity.succeed("test -f /tmp/observed/nomad-secret")
    identity.fail("test -e /tmp/observed/approle-login")

    gateway.succeed("ssh-keygen -L -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub | grep -q 'host certificate'")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub) = 400")
    gateway.succeed("test $(stat -c %U:%G /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub) = telchar:telchar")
    gateway.succeed("test $(cat /var/lib/telchar/credentials/nomad-token.candidate) = vault-nomad-token")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/host-ca.pub | cut -d' ' -f1) = " + host_ca_hash)
    client_ca_hash = identity.succeed("sha256sum /var/lib/vault-fixture/ssh-client-signer.pub").split()[0]
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/client-ca.pub | cut -d' ' -f1) = " + client_ca_hash)
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/nomad-token.candidate) = 400")
    gateway.succeed("test $(stat -c %U:%G /var/lib/telchar/credentials/nomad-token.candidate) = telchar:telchar")
    gateway.succeed("ssh-keygen -L -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | grep -q 'host certificate'")
    gateway.succeed("test $(cat /var/lib/telchar/credentials/nomad-token) = vault-nomad-token")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/nomad-token) = 400")
    ssh_candidate_hash = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub | cut -d' ' -f1").strip()
    nomad_candidate_hash = gateway.succeed("sha256sum /var/lib/telchar/credentials/nomad-token.candidate | cut -d' ' -f1").strip()
    identity.succeed("touch /tmp/observed/stall-imds")
    gateway.fail("timeout 12 systemctl start telchar-vault-aws-auth.service")
    gateway.succeed("test $(systemctl show telchar-vault-aws-auth.service -p TimeoutStartUSec --value) = 1min")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub | cut -d' ' -f1) = " + ssh_candidate_hash)
    gateway.succeed("test $(sha256sum /var/lib/telchar/credentials/nomad-token.candidate | cut -d' ' -f1) = " + nomad_candidate_hash)
    identity.succeed("rm /tmp/observed/stall-imds && touch /tmp/observed/oversized-imds")
    gateway.succeed("systemctl reset-failed telchar-vault-aws-auth.service")
    gateway.fail("timeout 12 systemctl start telchar-vault-aws-auth.service")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub | cut -d' ' -f1) = " + ssh_candidate_hash)
    gateway.succeed("test $(sha256sum /var/lib/telchar/credentials/nomad-token.candidate | cut -d' ' -f1) = " + nomad_candidate_hash)
    identity.succeed("rm /tmp/observed/oversized-imds")
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
