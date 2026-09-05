# Verifies atomic Nomad token renewal and narrow Telchar configuration reload.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
let
  nomadApi = pkgs.writeText "nomad-api.py" ''
    from http.server import BaseHTTPRequestHandler, HTTPServer

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            self.rfile.read(length)
            with open("/tmp/observed-token", "w") as output:
                output.write(self.headers.get("X-Nomad-Token", ""))
            body = b'{"EvalID":"evaluation-1","Warnings":""}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format, *args):
            pass

    HTTPServer(("0.0.0.0", 4646), Handler).serve_forever()
  '';
  buildDerivation = pkgs.writeText "nomad-renewal-build.nix" ''
    derivation {
      name = "nomad-renewal-build";
      system = builtins.currentSystem;
      builder = builtins.storePath "${pkgs.runtimeShell}";
      args = [ "-c" "sleep 60; touch $out" ];
    }
  '';
in
pkgs.testers.nixosTest {
  name = "telchar-nixos-nomad-credential-renewal";

  nodes = {
    postgres =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        services.postgresql = {
          enable = true;
          package = pkgs.postgresql;
          enableTCPIP = true;
          authentication = pkgs.lib.mkForce ''
            local all all trust
            host telchar telchar 0.0.0.0/0 trust
            host telchar telchar ::/0 trust
          '';
          ensureDatabases = [ "telchar" ];
          ensureUsers = [
            {
              name = "telchar";
              ensureDBOwnership = true;
            }
          ];
        };
        system.stateVersion = "26.05";
      };

    nomad-api =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        environment.systemPackages = [ pkgs.python3 ];
        system.stateVersion = "26.05";
      };

    client =
      { pkgs, ... }:
      {
        networking.extraHosts = "192.168.1.2 gateway";
        environment.systemPackages = [ pkgs.nix ];
        system.stateVersion = "26.05";
      };

    gateway =
      { pkgs, ... }:
      {
        imports = [ standaloneModule ];

        services.openssh.enable = true;
        networking.extraHosts = ''
          192.168.1.1 postgres
          192.168.1.2 nomad-api
        '';

        services.telchar = {
          package = telchar;
          database.url = "postgresql://telchar@postgres/telchar";
          nomad = {
            tokenFile = "/var/lib/telchar/credentials/nomad-token";
            renewal = {
              enable = true;
              candidateFile = "/var/lib/telchar/credentials/nomad-token.candidate";
            };
          };
          ingress.openssh = {
            hostKeyFile = "/etc/ssh/ssh_host_ed25519_key";
            authorizedKeysFile = "/var/lib/telchar/ssh/authorized_keys";
          };
          settings = {
            running_disconnect_policy = "detach-and-finish";
            backends.nomad = [
              {
                nomad-test = {
                  inherit system;
                  maximum_concurrent_builds = 1;
                  endpoint = "http://nomad-api:4646";
                  namespace = "telchar";
                  token_file = "/run/telchar/credentials/nomad-token";
                  driver = "raw_exec";
                  job_name_scope = "telchar-test";
                  poll_interval_seconds = 1;
                  runtime_limit_seconds = 60;
                  transfer_endpoint = "ws://gateway:7443/callback";
                  transfer_authentication = {
                    mode = "workload-identity";
                    issuer = "http://nomad-api:4646";
                    jwks_url = "http://nomad-api:4646/.well-known/jwks.json";
                    audience = "telchar-transfer";
                  };
                  store = {
                    mode = "daemon";
                    uri = "unix:///nix/var/nix/daemon-socket/socket";
                  };
                  transfer_limits = {
                    maximum_manifest_paths = 1024;
                    maximum_manifest_bytes = 1048576;
                    maximum_input_nar_bytes = 1073741824;
                    maximum_total_input_bytes = 8589934592;
                    maximum_output_nar_bytes = 1073741824;
                    maximum_total_output_bytes = 8589934592;
                    maximum_frame_metadata_bytes = 65536;
                    stream_buffer_bytes = 262144;
                    maximum_live_log_chunk_bytes = 65536;
                    live_log_queue_bytes = 1048576;
                    transfer_idle_timeout_seconds = 30;
                    setup_timeout_seconds = 300;
                    output_collection_timeout_seconds = 300;
                    maximum_connection_lifetime_seconds = 3600;
                    authentication_lifetime_seconds = 300;
                    clock_skew_seconds = 30;
                    nonce_retention_seconds = 600;
                    reconnect_timeout_seconds = 30;
                    maximum_diagnostic_bytes = 65536;
                  };
                  resources = {
                    cpu_mhz = 100;
                    memory_mb = 128;
                    disk_mb = 256;
                  };
                  driver_config.command = "${pkgs.coreutils}/bin/true";
                };
              }
            ];
          };
          environment = {
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };

        users.users.telchar.hashedPassword = "";
        systemd.tmpfiles.rules = [
          "d /var/lib/telchar/ssh 0700 telchar telchar -"
          "f /var/lib/telchar/ssh/authorized_keys 0600 telchar telchar -"
        ];
        system.stateVersion = "26.05";
      };
  };

  testScript = ''
    postgres.start()
    postgres.wait_for_unit("postgresql.service")
    nomad_api.start()
    nomad_api.succeed("systemd-run --unit=nomad-api ${pkgs.python3}/bin/python3 ${nomadApi}")
    nomad_api.wait_until_succeeds("ss -ltn | grep -q ':4646 '")

    gateway.start()
    gateway.succeed("systemctl stop telchar.service")
    gateway.succeed("systemctl reset-failed telchar.service")
    gateway.wait_for_unit("network-online.target")
    gateway.wait_until_succeeds("timeout 5 bash -c '</dev/tcp/postgres/5432'")
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/credentials")
    gateway.succeed("printf protected-nomad-token > /var/lib/telchar/credentials/nomad-token")
    gateway.succeed("chown telchar:telchar /var/lib/telchar/credentials/nomad-token")
    gateway.succeed("chmod 400 /var/lib/telchar/credentials/nomad-token")
    gateway.succeed("systemctl start telchar.service")
    gateway.wait_until_succeeds("systemctl is-active --quiet telchar.service && test -S /run/telchar/daemon.sock")
    gateway.succeed("systemctl reset-failed telchar-sshd.service && systemctl restart telchar-sshd.service")
    gateway.wait_for_unit("telchar-sshd.service")

    gateway.succeed("systemctl is-enabled nomad.service >/dev/null 2>&1 && exit 1 || true")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/nomad-token) = 400")
    gateway.succeed("test $(stat -c %U:%G /var/lib/telchar/credentials/nomad-token) = telchar:telchar")
    gateway.succeed("pid=$(systemctl show telchar.service -p MainPID --value); nsenter -t $pid -m -- test $(nsenter -t $pid -m -- stat -c %a /run/telchar/credentials/nomad-token) = 400")
    gateway.succeed("pid=$(systemctl show telchar.service -p MainPID --value); nsenter -t $pid -m -- test $(nsenter -t $pid -m -- stat -c %U /run/telchar/credentials/nomad-token) = telchar")
    client.start()
    client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
    ingress_key = client.succeed("cat /root/.ssh/telchar.pub").strip()
    gateway.succeed("printf '%s\\n' '" + ingress_key + "' > /var/lib/telchar/ssh/authorized_keys")
    client.wait_until_succeeds("ssh-keyscan -p 2222 192.168.1.2 > /root/.ssh/known_hosts 2>/dev/null")
    client.succeed("cp ${buildDerivation} /tmp/nomad-renewal-build.nix")
    derivation_path = client.succeed("nix-instantiate /tmp/nomad-renewal-build.nix").strip()
    derivation_export = client.succeed("nix-store --export '" + derivation_path + "' | base64 -w0").strip()
    gateway.succeed("printf '%s' '" + derivation_export + "' | base64 -d | nix-store --import >/dev/null")

    original_pid = gateway.succeed("systemctl show telchar.service -p MainPID --value").strip()
    gateway.succeed("umask 077; cat /proc/sys/kernel/random/uuid > /root/expected-token; cp /root/expected-token /var/lib/telchar/credentials/nomad-token.candidate; chown telchar:telchar /var/lib/telchar/credentials/nomad-token.candidate; chmod 400 /var/lib/telchar/credentials/nomad-token.candidate")
    expected_digest = gateway.succeed("tr -d '\\n' < /root/expected-token | sha256sum | cut -d ' ' -f1").strip()
    gateway.succeed("systemctl start telchar-nomad-credential-renewal.service")
    gateway.succeed("test $(systemctl show telchar.service -p MainPID --value) = " + original_pid)
    gateway.succeed("systemctl is-active --quiet telchar.service")
    gateway.succeed("test $(tr -d '\\n' < /var/lib/telchar/credentials/nomad-token | sha256sum | cut -d ' ' -f1) = " + expected_digest)

    gateway.succeed("install -d -m 700 /root/private-candidate && printf root-private-token > /root/private-candidate/nomad-token && rm -f /var/lib/telchar/credentials/nomad-token.candidate && ln -s /root/private-candidate/nomad-token /var/lib/telchar/credentials/nomad-token.candidate")
    gateway.fail("systemctl start telchar-nomad-credential-renewal.service")
    gateway.succeed("test $(tr -d '\\n' < /var/lib/telchar/credentials/nomad-token | sha256sum | cut -d ' ' -f1) = " + expected_digest)
    gateway.succeed("test $(systemctl show telchar.service -p MainPID --value) = " + original_pid)
    gateway.succeed("systemctl is-active --quiet telchar.service")
    gateway.succeed("rm /var/lib/telchar/credentials/nomad-token.candidate")

    gateway.succeed("printf invalid-nomad-token > /var/lib/telchar/credentials/nomad-token.candidate && chown telchar:telchar /var/lib/telchar/credentials/nomad-token.candidate && chmod 440 /var/lib/telchar/credentials/nomad-token.candidate")
    gateway.fail("systemctl start telchar-nomad-credential-renewal.service")
    gateway.succeed("systemctl is-failed --quiet telchar-nomad-credential-renewal.service")
    gateway.succeed("test $(tr -d '\\n' < /var/lib/telchar/credentials/nomad-token | sha256sum | cut -d ' ' -f1) = " + expected_digest)
    gateway.succeed("test $(systemctl show telchar.service -p MainPID --value) = " + original_pid)
    gateway.succeed("systemctl is-active --quiet telchar.service")

    build = "HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts -p 2222' nix --extra-experimental-features nix-command build --no-link --max-jobs 0 --builders 'ssh-ng://telchar@192.168.1.2 ${system} - 1 1' '" + derivation_path + "^*'"
    client.succeed("(" + build + " >/tmp/build.out 2>&1) & echo $! >/tmp/build.pid")
    nomad_api.wait_until_succeeds("test $(sha256sum /tmp/observed-token | cut -d ' ' -f1) = " + expected_digest, timeout=60)
    client.succeed("kill $(cat /tmp/build.pid) || true")
    status, output = gateway.execute("timeout 120 grep -rFl -f /root/expected-token /nix/store", timeout=130)
    assert status == 1, f"Store credential scan did not confirm absence: status={status} output={output}"
    assert output == "", f"Store credential scan emitted diagnostics: {output}"
  '';
}
