# Proves protected gateway state remains usable across service restart and VM reboot.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
pkgs.testers.nixosTest {
  name = "telchar-nixos-restart-reboot";

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

    client =
      { pkgs, ... }:
      {
        environment.systemPackages = [ pkgs.openssh ];
        system.stateVersion = "26.05";
      };

    gateway =
      { pkgs, ... }:
      {
        imports = [ standaloneModule ];

        networking.extraHosts = "192.168.1.1 postgres";
        services.openssh.enable = true;
        services.telchar = {
          package = telchar;
          database.url = "postgresql://telchar@postgres/telchar";
          callback = {
            enable = true;
            openFirewall = true;
          };
          ingress.openssh = {
            hostKeyFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key";
            authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
            hostCertificateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub";
            trustedUserCAKeysFile = "/var/lib/telchar/ssh/client-ca.pub";
            authorizedPrincipalsFile = "/var/lib/telchar/ssh/authorized_principals";
          };
          nomad.tokenFile = "/var/lib/telchar/credentials/nomad-token";
          settings = {
            backends = {
              local = {
                name = "local";
                inherit system;
                maximum_concurrent_builds = 1;
              };
              nomad = [
                {
                  restart-test = {
                    inherit system;
                    maximum_concurrent_builds = 1;
                    endpoint = "http://127.0.0.1:4646";
                    namespace = "telchar";
                    token_file = "/run/telchar/credentials/nomad-token";
                    driver = "raw_exec";
                    job_name_scope = "telchar-restart";
                    poll_interval_seconds = 1;
                    runtime_limit_seconds = 60;
                    transfer_endpoint = "ws://gateway:7443/callback";
                    transfer_authentication = {
                      mode = "hmac";
                      key_id = "restart-test";
                      secret_file = "/run/telchar/credentials/callback-secret";
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
              nomad_callback = {
                bind = "0.0.0.0:7443";
                public_url = "ws://gateway:7443/callback";
                maximum_connections = 8;
                maximum_header_bytes = 16384;
                maximum_body_bytes = 65536;
                authentication_request_timeout_seconds = 2;
                shutdown_drain_timeout_seconds = 2;
                maximum_jwks_bytes = 1048576;
                maximum_retained_nonces = 1024;
              };
            };
          };
          environment = {
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };

        environment.etc."ssh/authorized_keys.d/telchar".text = "";
        systemd.services.telchar.unitConfig.ConditionPathExists = "/var/lib/telchar/credentials/nomad-token";
        systemd.services.telchar-sshd.unitConfig.ConditionPathExists = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub";
        system.stateVersion = "26.05";
      };
  };

  testScript = ''
    postgres.start()
    postgres.wait_for_unit("postgresql.service")
    client.start()
    client.succeed("install -d -m 700 /root/.ssh")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/client-ca")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/client")
    client.succeed("ssh-keygen -q -s /root/.ssh/client-ca -I restart-client -n telchar-builder -V -1m:+30m /root/.ssh/client.pub")
    client_ca = client.succeed("cat /root/.ssh/client-ca.pub").strip()

    gateway.start(allow_reboot=True)
    gateway.wait_for_unit("multi-user.target")
    gateway.succeed("systemctl stop telchar.service telchar-sshd.service || true")
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/ssh /var/lib/telchar/credentials")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/ssh_host_ed25519_key")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/host-ca")
    gateway.succeed("ssh-keygen -q -s /var/lib/telchar/ssh/host-ca -I restart-host -h -n gateway -V -1m:+30m /var/lib/telchar/ssh/ssh_host_ed25519_key.pub")
    gateway.succeed("printf '%s\\n' '" + client_ca + "' > /var/lib/telchar/ssh/client-ca.pub")
    gateway.succeed("printf 'telchar-builder\\n' > /var/lib/telchar/ssh/authorized_principals")
    gateway.succeed("printf restart-nomad-token > /var/lib/telchar/credentials/nomad-token")
    gateway.succeed("printf restart-callback-secret > /var/lib/telchar/credentials/callback-secret")
    gateway.succeed("install -m 600 -o telchar -g telchar /dev/null /var/lib/telchar/restart-marker")
    gateway.succeed("chown -R telchar:telchar /var/lib/telchar/ssh /var/lib/telchar/credentials && chmod 600 /var/lib/telchar/ssh/ssh_host_ed25519_key /var/lib/telchar/ssh/host-ca && chmod 644 /var/lib/telchar/ssh/*.pub /var/lib/telchar/ssh/authorized_principals && chmod 400 /var/lib/telchar/credentials/nomad-token /var/lib/telchar/credentials/callback-secret")
    gateway.succeed("systemctl daemon-reload")
    gateway.succeed("mkdir -p /run/systemd/system/telchar.service.d; printf '[Service]\\nExecStartPre=/run/current-system/sw/bin/test ! -e /run/telchar-startup-blocked\\n' > /run/systemd/system/telchar.service.d/startup-test.conf")
    gateway.succeed("touch /run/telchar-startup-blocked; systemctl daemon-reload")
    gateway.succeed("systemctl reset-failed telchar.service telchar-sshd.service; systemctl start --no-block telchar.service telchar-sshd.service")
    gateway.wait_until_succeeds("journalctl -u telchar-sshd.service -b --no-pager | grep -q \"failed with result 'dependency'\"")
    gateway.fail("ss -ltn | grep -q ':2222 '")
    gateway.succeed("rm /run/telchar-startup-blocked")
    gateway.wait_until_succeeds("systemctl is-active --quiet telchar.service", timeout=30)
    gateway.wait_until_succeeds("systemctl is-active --quiet telchar-sshd.service", timeout=30)

    key_hash = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key | cut -d' ' -f1").strip()
    certificate_hash = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | cut -d' ' -f1").strip()
    ca_hash = gateway.succeed("sha256sum /var/lib/telchar/ssh/client-ca.pub | cut -d' ' -f1").strip()
    principal_hash = gateway.succeed("sha256sum /var/lib/telchar/ssh/authorized_principals | cut -d' ' -f1").strip()
    token_hash = gateway.succeed("sha256sum /var/lib/telchar/credentials/nomad-token | cut -d' ' -f1").strip()

    def assert_usable():
        gateway.succeed("systemctl is-active --quiet telchar.service && systemctl is-active --quiet telchar-sshd.service")
        gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
        gateway.succeed("ss -ltn | grep -q ':2222 '")
        gateway.succeed("ss -ltn | grep -q ':7443 '")
        gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key | cut -d' ' -f1) = " + key_hash)
        gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | cut -d' ' -f1) = " + certificate_hash)
        gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/client-ca.pub | cut -d' ' -f1) = " + ca_hash)
        gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/authorized_principals | cut -d' ' -f1) = " + principal_hash)
        gateway.succeed("test $(sha256sum /var/lib/telchar/credentials/nomad-token | cut -d' ' -f1) = " + token_hash)
        gateway.succeed("test -f /var/lib/telchar/restart-marker")
        client.succeed("timeout 10 ssh -vv -p 2222 -i /root/.ssh/client -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar@gateway </dev/null >/tmp/restart-auth.out 2>&1 || true; grep -q 'Authenticated to gateway' /tmp/restart-auth.out")
        client.succeed("timeout 5 bash -c '</dev/tcp/gateway/7443'")

    assert_usable()
    gateway.succeed("systemctl restart telchar.service")
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_unit("telchar-sshd.service")
    assert_usable()

    gateway.succeed("systemctl kill --kill-whom=main --signal=SIGKILL telchar.service")
    gateway.wait_until_succeeds("systemctl is-active --quiet telchar.service && ss -ltn | grep -q ':7443 '", timeout=120)
    gateway.wait_until_succeeds("systemctl is-active --quiet telchar-sshd.service", timeout=30)
    gateway.wait_for_open_port(2222)
    assert_usable()

    gateway.reboot()
    gateway.wait_for_unit("multi-user.target")
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_unit("telchar-sshd.service")
    assert_usable()
  '';
}
