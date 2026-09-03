# Verifies protected external PostgreSQL configuration with hostname-verified TLS.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
pkgs.testers.nixosTest {
  name = "telchar-nixos-postgresql-tls";

  nodes = {
    postgres =
      { pkgs, ... }:
      {
        networking = {
          hostName = "postgres";
          domain = "database.test";
          firewall.allowedTCPPorts = [ 5432 ];
        };
        environment.systemPackages = [ pkgs.openssl ];
        systemd.services.postgresql-tls-material = {
          wantedBy = [ "postgresql.service" ];
          before = [ "postgresql.service" ];
          requiredBy = [ "postgresql.service" ];
          path = [ pkgs.openssl ];
          serviceConfig.Type = "oneshot";
          script = ''
            set -eu
            install -d -m 700 -o postgres -g postgres /var/lib/postgresql/tls
            openssl genrsa -out /var/lib/postgresql/tls/ca.key 2048
            openssl req -x509 -new -key /var/lib/postgresql/tls/ca.key \
              -out /var/lib/postgresql/tls/ca.crt -days 1 -subj /CN=telchar-test-ca
            openssl genrsa -out /var/lib/postgresql/tls/server.key 2048
            openssl req -new -key /var/lib/postgresql/tls/server.key \
              -out /var/lib/postgresql/tls/server.csr -subj /CN=postgres.database.test
            printf 'subjectAltName=DNS:postgres.database.test\n' >/var/lib/postgresql/tls/server.ext
            openssl x509 -req -in /var/lib/postgresql/tls/server.csr \
              -CA /var/lib/postgresql/tls/ca.crt -CAkey /var/lib/postgresql/tls/ca.key \
              -CAcreateserial -out /var/lib/postgresql/tls/server.crt -days 1 \
              -extfile /var/lib/postgresql/tls/server.ext
            cp /var/lib/postgresql/tls/ca.crt /tmp/shared/postgresql-ca.crt
            chown -R postgres:postgres /var/lib/postgresql/tls
            chmod 600 /var/lib/postgresql/tls/*.key
          '';
        };
        services.postgresql = {
          enable = true;
          package = pkgs.postgresql;
          enableTCPIP = true;
          settings = {
            ssl = "on";
            ssl_cert_file = "/var/lib/postgresql/tls/server.crt";
            ssl_key_file = "/var/lib/postgresql/tls/server.key";
          };
          authentication = pkgs.lib.mkForce ''
            local all all trust
            hostssl telchar telchar 0.0.0.0/0 trust
            hostssl telchar telchar ::/0 trust
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

    gateway =
      { pkgs, ... }:
      {
        imports = [ standaloneModule ];
        services.openssh.enable = true;
        networking.extraHosts = ''
          192.168.1.1 postgres.database.test
          192.168.1.1 wrong.database.test
        '';
        services.telchar = {
          package = telchar;
          database = {
            urlFile = "/var/lib/telchar/credentials/database-url";
            rootCertificateFile = "/var/lib/telchar/credentials/postgresql-ca.crt";
          };
          ingress.openssh = {
            hostKeyFile = "/etc/ssh/ssh_host_ed25519_key";
            authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
          };
          settings.backends.local = {
            name = "local";
            inherit system;
            maximum_concurrent_builds = 1;
          };
          environment = {
            TELCHAR_DATABASE_URL = "postgresql://telchar@postgres.database.test/telchar?sslmode=disable";
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };
        environment.etc."ssh/authorized_keys.d/telchar".text = "";
        systemd.services.telchar.serviceConfig.Restart = pkgs.lib.mkForce "no";
        systemd.services.postgresql-client-material = {
          wantedBy = [ "multi-user.target" ];
          before = [ "telchar.service" ];
          requiredBy = [ "telchar.service" ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            set -eu
            while [ ! -s /tmp/shared/postgresql-ca.crt ]; do sleep 0.1; done
            install -d -m 700 -o telchar -g telchar /var/lib/telchar/credentials
            install -m 400 -o telchar -g telchar /tmp/shared/postgresql-ca.crt /var/lib/telchar/credentials/postgresql-ca.crt
            printf '%s\n' 'postgresql://telchar@postgres.database.test/telchar?sslmode=verify-full&sslrootcert=/var/lib/telchar/credentials/postgresql-ca.crt' > /var/lib/telchar/credentials/database-url
            chown telchar:telchar /var/lib/telchar/credentials/database-url
            chmod 400 /var/lib/telchar/credentials/database-url
          '';
        };
        system.stateVersion = "26.05";
      };
  };

  testScript = ''
    postgres.start()
    postgres.wait_for_unit("postgresql.service")
    gateway.start()
    gateway.wait_for_unit("telchar.service")
    gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
    gateway.succeed("test \"$(systemctl show telchar.service -p Environment --value)\" != *postgres.database.test*")
    gateway.succeed("test \"$(systemctl show telchar.service -p Environment --value)\" != *TELCHAR_DATABASE_URL*")
    gateway.succeed("test $(stat -c %a /var/lib/telchar/credentials/database-url) = 400")
    gateway.succeed("case $(readlink -f /var/lib/telchar/credentials/database-url) in /nix/store/*) exit 1;; esac")
    gateway.fail("nix-store -qR /run/current-system | xargs grep -I -l -m1 postgres.database.test 2>/dev/null")
    gateway.succeed("sed -i 's/postgres.database.test/wrong.database.test/' /var/lib/telchar/credentials/database-url")
    gateway.succeed("systemctl restart telchar.service")
    gateway.wait_until_succeeds("systemctl is-failed --quiet telchar.service")
    gateway.succeed("journalctl -u telchar.service --no-pager | grep -q 'database migration failed'")
    gateway.succeed("sed -i 's/wrong.database.test/postgres.database.test/' /var/lib/telchar/credentials/database-url")
    gateway.succeed("printf invalid-ca > /var/lib/telchar/credentials/postgresql-ca.crt")
    gateway.succeed("systemctl reset-failed telchar.service && systemctl start telchar.service")
    gateway.wait_until_succeeds("systemctl is-failed --quiet telchar.service")
  '';
}
