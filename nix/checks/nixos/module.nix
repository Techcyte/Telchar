# Exercises packaged service startup, protected database configuration, telemetry, and SSH ingress.
{
  pkgs,
  system,
  telchar,
  telcharModule,
  ...
}:
{
  nixos-module = pkgs.testers.nixosTest {
    name = "telchar-nixos-module";
    nodes.gateway =
      { pkgs, ... }:
      {
        imports = [
          telcharModule
          (import ../../../tests/nixos/common.nix { inherit pkgs telchar; }).collectorModule
        ];
        systemd.services.telchar.serviceConfig.Restart = pkgs.lib.mkForce "no";
        services.postgresql = {
          enableTCPIP = true;
          settings = {
            ssl = "on";
            ssl_cert_file = "/var/lib/postgresql/tls/server.crt";
            ssl_key_file = "/var/lib/postgresql/tls/server.key";
          };
          authentication = pkgs.lib.mkAfter ''
            hostssl telchar telchar 127.0.0.1/32 trust
          '';
        };
        # Runtime-only credentials exercise service ownership and ExecStartPre wiring.
        # Certificate rejection matrices live in the real-PostgreSQL Rust suites.
        systemd.services.postgresql-client-material = {
          before = [
            "postgresql.service"
            "telchar.service"
          ];
          requiredBy = [
            "postgresql.service"
            "telchar.service"
          ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          path = [ pkgs.openssl ];
          script = ''
            set -eu
            umask 077
            install -d -m 700 -o postgres -g postgres /var/lib/postgresql/tls
            cd /var/lib/postgresql/tls
            openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=telchar-test-ca -keyout ca.key -out ca.crt 2>ca.log
            openssl req -newkey rsa:2048 -nodes -subj /CN=localhost -keyout server.key -out server.csr 2>request.log
            printf 'subjectAltName=DNS:localhost\n' > server.ext
            openssl x509 -req -days 1 -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -extfile server.ext -out server.crt 2>signing.log
            chown -R postgres:postgres /var/lib/postgresql/tls
            install -d -m 700 -o telchar -g telchar /var/lib/telchar/credentials
            install -m 400 -o telchar -g telchar ca.crt /var/lib/telchar/credentials/database-ca.crt
            install -m 400 -o telchar -g telchar ca.crt /var/lib/telchar/credentials/native-ca.crt
            openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=unrelated-ca -keyout unrelated.key -out unrelated.crt 2>unrelated.log
            printf '%s\n' 'postgresql://telchar@localhost/telchar?sslmode=verify-full&sslrootcert=/var/lib/telchar/credentials/database-ca.crt' > /var/lib/telchar/credentials/database-url
            chown telchar:telchar /var/lib/telchar/credentials/database-url
            chmod 400 /var/lib/telchar/credentials/database-url
          '';
        };
        networking.firewall.enable = false;
        services.openssh.enable = true;
        services.openssh.ports = [ 22 ];
        services.telchar = {
          enable = true;
          package = telchar;
          frontendUid = 995;
          database = {
            manage = true;
            urlFile = "/var/lib/telchar/credentials/database-url";
            rootCertificateFile = "/var/lib/telchar/credentials/database-ca.crt";
          };
          gatewayStore.manageTrustedUser = true;
          gatewayStore.manageGcRootDirectory = true;
          ingress.openssh = {
            enable = true;
            port = 2222;
            hostKeyFile = "/etc/ssh/ssh_host_ed25519_key";
            authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
          };
          settings = {
            running_disconnect_policy = "detach-and-finish";
            backends.local = {
              name = "local";
              system = system;
              maximum_concurrent_builds = 1;
            };
          };
          environment = {
            TELCHAR_DATABASE_URL = "postgresql://unusable/telchar?sslmode=disable";
            OTEL_EXPORTER_OTLP_ENDPOINT = "http://localhost:4317";
            SSL_CERT_FILE = "/var/lib/telchar/credentials/native-ca.crt";
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };
        environment.etc."ssh/authorized_keys.d/telchar".text = ''
          ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGQ5k8KfV+TWbrZG7MBXn9cKbIYB1vLLtvbCeK6ucvE3 telchar-module-test
          ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG31m7DcBk/wDNv27MOMNXD9Yk6tfhpj1dBl1VOdnyou telchar-module-second-test
        '';
        system.stateVersion = "26.05";
      };
    testScript = ''
      start_all()
      gateway.wait_for_unit("postgresql.service")
      gateway.wait_for_unit("telchar.service")
      # The installed daemon must consume protected TLS credentials, not the environment decoy.
      gateway.succeed("sudo -u postgres psql -Atc \"select count(*) from pg_stat_ssl join pg_stat_activity using (pid) where usename = 'telchar' and ssl\" | grep -Eq '^[1-9][0-9]*$'")
      gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/database-url) = 400")
      gateway.succeed("systemctl show telchar.service -p Environment --value | ${pkgs.python3}/bin/python3 -c 'import shlex, sys; assert not any(value.startswith(\"TELCHAR_DATABASE_URL=\") for value in shlex.split(sys.stdin.read()))'")
      # Telemetry must leave the packaged service and reach a real OTLP collector.
      gateway.wait_for_unit("otelcol.service")
      gateway.wait_until_succeeds("test -s /var/lib/telchar-otlp/records.json", timeout=60)
      gateway.succeed("systemctl is-active sshd.service")
      gateway.succeed("systemctl is-active telchar-sshd.service")
      gateway.succeed("systemctl is-active telchar.service || { systemctl status telchar.service --no-pager >&2; journalctl -u telchar.service --no-pager >&2; exit 1; }")
      gateway.succeed("sshd -T | grep -qx 'port 22'")
      gateway.succeed("${pkgs.openssh}/bin/sshd -T -f /etc/telchar/sshd_config | grep -qx 'port 2222'")
      gateway.succeed("! grep -q 'telchar-forced-command' /etc/ssh/sshd_config")
      gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
      gateway.succeed("test $(stat -c %a /run/telchar) = 700")
      gateway.succeed("sudo -u postgres psql -Atc \"select 1 from pg_database where datname = 'telchar'\" | grep -qx 1")
      gateway.succeed("systemctl show telchar.service -p User --value | grep -qx telchar")
      gateway.succeed("grep -q '^ForceCommand /nix/store/' /etc/telchar/sshd_config")
      gateway.succeed("grep -q '^ExposeAuthInfo yes$' /etc/telchar/sshd_config")
      gateway.succeed("forced_command=$(awk '/^ForceCommand / { print $2; exit }' /etc/telchar/sshd_config); ! grep -Fq '/etc/ssh/authorized_keys.d/telchar' \"$forced_command\" && grep -Fq 'SSH_USER_AUTH' \"$forced_command\" && grep -Fq 'ssh-keygen -lf -' \"$forced_command\"")
      # ExecStartPre must reject decoy TLS parameters before the daemon can start.
      gateway.succeed("cp /var/lib/telchar/credentials/database-url /var/lib/telchar/credentials/verified-url; printf '%s\\n' 'postgresql://telchar@localhost/telchar?sslmode=disable&options=sslmode=verify-full&application_name=sslrootcert=/var/lib/telchar/credentials/database-ca.crt' > /var/lib/telchar/credentials/database-url")
      gateway.fail("systemctl restart telchar.service")
      gateway.succeed("systemctl is-failed --quiet telchar.service")
      gateway.succeed("journalctl -u telchar.service --no-pager | grep -q 'database URL must use effective sslmode=verify-full'")
      # An explicit authority must not silently fall back to the valid native trust file.
      gateway.succeed("cp /var/lib/telchar/credentials/verified-url /var/lib/telchar/credentials/database-url; install -m 400 -o telchar -g telchar /var/lib/postgresql/tls/unrelated.crt /var/lib/telchar/credentials/database-ca.crt")
      gateway.succeed("systemctl reset-failed telchar.service && systemctl start telchar.service")
      gateway.wait_until_succeeds("systemctl is-failed --quiet telchar.service")
      gateway.succeed("journalctl -u telchar.service --no-pager | grep -q 'database migration failed'")
      gateway.succeed("install -m 400 -o telchar -g telchar /var/lib/postgresql/tls/ca.crt /var/lib/telchar/credentials/database-ca.crt; systemctl reset-failed telchar.service && systemctl start telchar.service")
      gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
    '';
  };
  nixos-restricted-ssh-ingress =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkTest {
      name = "telchar-nixos-restricted-ssh-ingress";
      restrictedIngress = true;
      includeCollector = true;
      testScript = ''
        start_all()
        otlp_collector.wait_for_open_port(4317)
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_for_unit("sshd.service")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ssh_options = "-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar-ingress@gateway"
        stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout 30 nix --extra-experimental-features nix-command --store ssh-ng://telchar-ingress@gateway store info > /tmp/nix-store-info 2>&1")
        stock_client.succeed("grep -q 'Version: telchar' /tmp/nix-store-info || { cat /tmp/nix-store-info >&2; exit 1; }")
        stock_client.succeed("timeout 10 ssh " + ssh_options + " arbitrary-command >/dev/null 2>&1 || true")
        gateway.succeed("grep -q '^original_command=arbitrary-command$' /tmp/telchar-forced-command-evidence")
        stock_client.succeed("TELCHAR_AUTHENTICATED_KEY=spoofed timeout 10 ssh -o SendEnv=TELCHAR_AUTHENTICATED_KEY " + ssh_options + " ignored >/dev/null 2>&1 || true")
        gateway.succeed("grep -q '^client_supplied_key=$' /tmp/telchar-forced-command-evidence && ! grep -q '^authenticated_key=spoofed$' /tmp/telchar-forced-command-evidence")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -tt " + ssh_options + " true >/tmp/pty.out 2>&1; echo $?) -ne 0")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -R 127.0.0.1:22346:127.0.0.1:22 -N " + ssh_options + " >/tmp/remote-forward.out 2>&1; echo $?) -ne 0")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -L 127.0.0.1:22345:127.0.0.1:22 -N " + ssh_options + " >/tmp/local-forward.out 2>&1; echo $?) -ne 0")
      '';
    };
}
