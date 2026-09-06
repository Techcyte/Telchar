# A real Nomad server/client pair, optionally executing the allocation worker through Telchar.
{ pkgs, ingress }:
let
  inherit (ingress) restrictedIngressGatewayModule restrictedIngressClientModule;
in
{
  mkNomadGatewayTest =
    {
      name,
      worker,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        stock-client = restrictedIngressClientModule;
        gateway =
          { ... }:
          {
            imports = [ restrictedIngressGatewayModule ];
            networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkOverride 0 [
              {
                address = "192.168.1.3";
                prefixLength = 24;
              }
            ];
            systemd.services.telchar-daemon.wantedBy = pkgs.lib.mkForce [ ];
            systemd.services.telchar-daemon.environment.TELCHAR_CONFIG =
              pkgs.lib.mkForce "/var/lib/telchar-import/telchar.toml";
            environment.etc."telchar/nomad.toml".text = ''
              running_disconnect_policy = "detach-and-finish"

              [[backends.nomad]]
              [backends.nomad.nomad-primary]
              system = "${pkgs.stdenv.hostPlatform.system}"
              maximum_concurrent_builds = 1
              endpoint = "http://192.168.1.1:4646"
              namespace = "telchar"
              driver = "raw_exec"
              job_name_scope = "telchar-gateway"
              poll_interval_seconds = 1
              runtime_limit_seconds = 120

              transfer_endpoint = "ws://192.168.1.3:7443/callback"

              [backends.nomad.nomad-primary.transfer_authentication]
              mode = "hmac"
              key_id = "fixture"
              secret_file = "/var/lib/telchar-import/nomad-transfer.key"

              [backends.nomad.nomad-primary.store]
              mode = "daemon"
              uri = "unix:///nix/var/nix/daemon-socket/socket"

              [backends.nomad.nomad-primary.transfer_limits]
              maximum_manifest_paths = 1024
              maximum_manifest_bytes = 1048576
              maximum_input_nar_bytes = 1073741824
              maximum_total_input_bytes = 8589934592
              maximum_output_nar_bytes = 1073741824
              maximum_total_output_bytes = 8589934592
              maximum_frame_metadata_bytes = 65536
              stream_buffer_bytes = 262144
              maximum_live_log_chunk_bytes = 65536
              live_log_queue_bytes = 1048576
              transfer_idle_timeout_seconds = 30
              setup_timeout_seconds = 300
              output_collection_timeout_seconds = 300
              maximum_connection_lifetime_seconds = 3600
              authentication_lifetime_seconds = 300
              clock_skew_seconds = 30
              nonce_retention_seconds = 600
              reconnect_timeout_seconds = 30
              maximum_diagnostic_bytes = 65536

              [backends.nomad.nomad-primary.resources]
              cpu_mhz = 100
              memory_mb = 128
              disk_mb = 256

              [backends.nomad.nomad-primary.driver_config]
              command = "${worker}/bin/telchar-nomad-worker"
            '';
          };
        nomad-server =
          { ... }:
          {
            networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkOverride 0 [
              {
                address = "192.168.1.1";
                prefixLength = 24;
              }
            ];
            networking.firewall.enable = false;
            system.stateVersion = "26.05";
            environment.variables.NOMAD_ADDR = "http://192.168.1.1:4646";
            services.nomad = {
              enable = true;
              enableDocker = false;
              dropPrivileges = false;
              settings = {
                bind_addr = "0.0.0.0";
                advertise = {
                  http = "192.168.1.1:4646";
                  rpc = "192.168.1.1:4647";
                  serf = "192.168.1.1:4648";
                };
                server = {
                  enabled = true;
                  bootstrap_expect = 1;
                };
              };
            };
          };
        nomad-client =
          { ... }:
          {
            networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkOverride 0 [
              {
                address = "192.168.1.2";
                prefixLength = 24;
              }
            ];
            networking.firewall.enable = false;
            system.stateVersion = "26.05";
            environment.variables.TELCHAR_NIX_STORE_URI = "local";
            services.nomad = {
              enable = true;
              enableDocker = false;
              dropPrivileges = false;
              extraPackages = [
                pkgs.bash
                worker
              ];
              settings = {
                bind_addr = "0.0.0.0";
                advertise = {
                  http = "192.168.1.2:4646";
                  rpc = "192.168.1.2:4647";
                  serf = "192.168.1.2:4648";
                };
                client = {
                  enabled = true;
                  servers = [ "192.168.1.1:4647" ];
                  options = {
                    "driver.raw_exec.enable" = "1";
                  };
                };
              };
            };
          };
      };
      testScript = ''
        start_all()
        gateway.wait_for_unit("postgresql.service")
        nomad_server.wait_for_unit("nomad.service")
        nomad_client.wait_for_unit("nomad.service")
        nomad_server.wait_until_succeeds("nomad operator raft list-peers | grep -q true", timeout=60)
        nomad_server.wait_until_succeeds("nomad namespace apply telchar", timeout=60)
        nomad_server.wait_until_succeeds("nomad node status -json | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].Status == \"ready\"'", timeout=60)
        gateway.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-import && install -m 600 -o telchar-ingress -g telchar /etc/telchar/nomad.toml /var/lib/telchar-import/telchar.toml && printf fixture-transfer-secret > /var/lib/telchar-import/nomad-transfer.key && chown telchar-ingress:telchar /var/lib/telchar-import/nomad-transfer.key && chmod 600 /var/lib/telchar-import/nomad-transfer.key")
        gateway.succeed("systemctl start telchar-daemon.service")
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        ingress_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + ingress_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ${testScript}
      '';
    };

  mkNomadFixtureTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        nomad-server =
          { ... }:
          {
            networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkOverride 0 [
              {
                address = "192.168.1.1";
                prefixLength = 24;
              }
            ];
            networking.firewall.enable = false;
            system.stateVersion = "26.05";
            environment.variables.NOMAD_ADDR = "http://192.168.1.1:4646";
            services.nomad = {
              enable = true;
              enableDocker = false;
              dropPrivileges = false;
              settings = {
                bind_addr = "0.0.0.0";
                advertise = {
                  http = "192.168.1.1:4646";
                  rpc = "192.168.1.1:4647";
                  serf = "192.168.1.1:4648";
                };
                server = {
                  enabled = true;
                  bootstrap_expect = 1;
                };
              };
            };
          };
        nomad-client =
          { ... }:
          {
            networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkOverride 0 [
              {
                address = "192.168.1.2";
                prefixLength = 24;
              }
            ];
            networking.firewall.enable = false;
            system.stateVersion = "26.05";
            services.nomad = {
              enable = true;
              enableDocker = false;
              dropPrivileges = false;
              extraPackages = [ pkgs.bash ];
              settings = {
                bind_addr = "0.0.0.0";
                advertise = {
                  http = "192.168.1.2:4646";
                  rpc = "192.168.1.2:4647";
                  serf = "192.168.1.2:4648";
                };
                client = {
                  enabled = true;
                  servers = [ "192.168.1.1:4647" ];
                  options = {
                    "driver.raw_exec.enable" = "1";
                  };
                };
              };
            };
          };
      };
      testScript = ''
        start_all()
        nomad_server.wait_for_unit("nomad.service")
        nomad_client.wait_for_unit("nomad.service")
        nomad_server.wait_until_succeeds("nomad operator raft list-peers | grep -q true", timeout=60)
        nomad_server.wait_until_succeeds("nomad node status -json | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].Status == \"ready\"'", timeout=60)
        ${testScript}
      '';
    };

}
