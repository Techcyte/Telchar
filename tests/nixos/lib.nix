{ pkgs, telchar }:
let
  machineModule =
    {
      role,
      extraConfig ? { },
    }:
    { ... }:
    {
      networking.firewall.enable = false;
      system.stateVersion = "26.05";
      environment.etc.telchar-test-role.text = role;
    }
    // extraConfig;

  gatewayModule = machineModule {
    role = "gateway";
    extraConfig = {
      systemd.services.telchar = {
        description = "Telchar integration service";
        wantedBy = [ "multi-user.target" ];
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        environment.OTEL_EXPORTER_OTLP_ENDPOINT = "http://otlp-collector:4317";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${telchar}/bin/telchar";
        };
      };
      systemd.services.telchar-artifacts-failure = {
        description = "Telchar controlled artifact failure";
        serviceConfig = {
          Type = "oneshot";
          Environment = "TELCHAR_TEST_SECRET=not-for-artifacts";
          ExecStart = "${pkgs.coreutils}/bin/false";
        };
      };
      systemd.services.telchar-artifacts = {
        description = "Telchar integration artifact capture";
        after = [ "telchar-artifacts-failure.service" ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = pkgs.writeShellScript "capture-telchar-artifacts" ''
            set -eu
            directory=/var/lib/telchar-artifacts
            rm -rf "$directory"
            mkdir -p "$directory"
            journalctl -u telchar.service -u telchar-artifacts-failure.service -n 200 --no-pager \
              | ${pkgs.gnused}/bin/sed 's/TELCHAR_TEST_SECRET=[^ ]*/TELCHAR_TEST_SECRET=[REDACTED]/g; s/not-for-artifacts/[REDACTED]/g' \
              > "$directory/journal.log"
            systemctl show telchar.service telchar-artifacts-failure.service --no-pager \
              --property=Id,LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus \
              | ${pkgs.gnused}/bin/sed 's/TELCHAR_TEST_SECRET=[^ ]*/TELCHAR_TEST_SECRET=[REDACTED]/g; s/not-for-artifacts/[REDACTED]/g' \
              > "$directory/machine-state.json"
            test "$(wc -c < "$directory/journal.log")" -le 65536
            test "$(wc -c < "$directory/machine-state.json")" -le 65536
            ! grep -q 'not-for-artifacts' "$directory/journal.log" "$directory/machine-state.json"
          '';
        };
      };
    };
  };

  stockClientModule = machineModule {
    role = "stock-client";
    extraConfig = {
      environment.systemPackages = [ pkgs.nix ];
    };
  };

  restrictedIngressGatewayModule = machineModule {
    role = "gateway";
    extraConfig = {
      environment.systemPackages = [ telchar ];
      services.postgresql = {
        enable = true;
        package = pkgs.postgresql;
        ensureDatabases = [ "telchar-ingress" ];
        ensureUsers = [
          {
            name = "telchar-ingress";
            ensureDBOwnership = true;
          }
        ];
      };
      services.openssh = {
        enable = true;
        settings = {
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PermitRootLogin = "prohibit-password";
          PermitTTY = false;
          AllowTcpForwarding = false;
          AllowAgentForwarding = false;
          X11Forwarding = false;
          PermitUserEnvironment = false;
        };
      };
      users.users.telchar-ingress = {
        isSystemUser = true;
        uid = 995;
        group = "telchar";
        home = "/var/lib/telchar-ingress";
        createHome = true;
        shell = "${pkgs.bashInteractive}/bin/bash";
      };
      users.groups.telchar = { };
      nix.settings.trusted-users = [
        "root"
        "telchar-ingress"
      ];
      systemd.services.nix-daemon.environment.PATH =
        pkgs.lib.mkForce "/run/telchar-direct-bin:/run/current-system/sw/bin";
      systemd.services.telchar-daemon = {
        description = "Telchar integration daemon";
        wantedBy = [ "multi-user.target" ];
        after = [
          "network-online.target"
          "postgresql.service"
        ];
        wants = [ "network-online.target" ];
        requires = [ "postgresql.service" ];
        environment = {
          OTEL_EXPORTER_OTLP_ENDPOINT = "http://otlp-collector:4317";
          TELCHAR_DATABASE_URL = "postgresql://telchar-ingress@/telchar-ingress?host=/run/postgresql";
          TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
          TELCHAR_GATEWAY_STORE_URI = "unix:///nix/var/nix/daemon-socket/socket";
          TMPDIR = "/var/lib/telchar-import";
          TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = "/var/lib/telchar-gc-roots";
          TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
          NIX_CONFIG = ''
            post-build-hook =
            substituters =
          '';
        };
        before = [ "sshd.service" ];
        serviceConfig = {
          User = "telchar-ingress";
          Group = "telchar";
          RuntimeDirectory = "telchar";
          RuntimeDirectoryMode = "0700";
          StateDirectory = [
            "telchar-import"
            "telchar-gc-roots"
          ];
          StateDirectoryMode = "0700";
          ExecStart = "${telchar}/bin/telchar daemon --socket /run/telchar/daemon.sock --frontend-uid 995";
        };
      };
      environment.etc."telchar/telchar.toml".text = ''
        [backends.local]
        name = "local"
        system = "${pkgs.stdenv.hostPlatform.system}"
        maximum_concurrent_builds = 1
      '';
      environment.etc."telchar/forced-command" = {
        mode = "0555";
        text = ''
          #!${pkgs.runtimeShell}
          set -eu
          fingerprint="$(${pkgs.openssh}/bin/ssh-keygen -lf /var/lib/telchar-ingress/.ssh/authorized_keys | ${pkgs.gawk}/bin/awk '{print $2}')"
          {
            printf 'original_command=%s\n' "''${SSH_ORIGINAL_COMMAND-}"
            printf 'authenticated_key=%s\n' "$fingerprint"
            printf 'client_supplied_key=%s\n' "''${TELCHAR_AUTHENTICATED_KEY-}"
            printf 'agent_socket=%s\n' "''${SSH_AUTH_SOCK-}"
            printf 'display=%s\n' "''${DISPLAY-}"
          } > /run/telchar/forced-command-evidence
          exec env OTEL_EXPORTER_OTLP_ENDPOINT=http://otlp-collector:4317 TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock TELCHAR_AUTHENTICATED_KEY="$fingerprint" ${telchar}/bin/telchar serve-stdio
        '';
      };
      environment.etc."ssh/sshd_config.d/telchar-test.conf".text = ''
        Match User telchar-ingress
          AuthorizedKeysFile /var/lib/telchar-ingress/.ssh/authorized_keys
          DisableForwarding yes
          PermitTTY no
          PermitUserEnvironment no
      '';
    };
  };

  restrictedIngressClientModule = machineModule {
    role = "stock-client";
    extraConfig = {
      environment.systemPackages = [
        pkgs.nix
        pkgs.openssh
      ];
    };
  };

  staticSshBuilderModule =
    {
      role ? "static-ssh-builder",
      account ? "telchar-builder",
      uid ? 994,
      evidence ? "/var/lib/telchar-builder/forced-command-evidence",
      systemFeatures ? [ ],
    }:
    machineModule {
      inherit role;
      extraConfig = {
        environment.systemPackages = [ pkgs.nix ];
        services.openssh = {
          enable = true;
          settings = {
            PasswordAuthentication = false;
            KbdInteractiveAuthentication = false;
            PermitRootLogin = "no";
            PermitTTY = false;
            AllowTcpForwarding = false;
            AllowAgentForwarding = false;
            X11Forwarding = false;
            PermitUserEnvironment = false;
          };
        };
        users.users.${account} = {
          isSystemUser = true;
          inherit uid;
          group = account;
          home = "/var/lib/${account}";
          createHome = true;
          shell = "${pkgs.bashInteractive}/bin/bash";
        };
        users.groups.${account} = { };
        nix.settings = {
          trusted-users = [
            "root"
            account
          ];
          system-features = systemFeatures;
        };
        environment.etc."telchar-static-ssh/forced-command" = {
          mode = "0555";
          text = ''
            #!${pkgs.runtimeShell}
            set -eu
            evidence=${evidence}
            printf 'original_command=%s agent_socket=%s display=%s\n' \
              "''${SSH_ORIGINAL_COMMAND-}" "''${SSH_AUTH_SOCK-}" "''${DISPLAY-}" >> "$evidence"
            case "''${SSH_ORIGINAL_COMMAND-}" in
              "nix-daemon --stdio") exec ${pkgs.nix}/bin/nix-daemon --stdio ;;
              "*/nix-daemon --stdio") exec ${pkgs.nix}/bin/nix-daemon --stdio ;;
              *) exit 126 ;;
            esac
          '';
        };
        systemd.tmpfiles.rules = [
          "f ${evidence} 0600 ${account} ${account} -"
        ];
        environment.etc."ssh/sshd_config.d/telchar-static-builder.conf".text = ''
          Match User ${account}
            AuthorizedKeysFile /var/lib/${account}/.ssh/authorized_keys
            ForceCommand /etc/telchar-static-ssh/forced-command
            DisableForwarding yes
            PermitTTY no
            PermitUserEnvironment no
        '';
      };
    };

  staticSshClientModule = machineModule {
    role = "static-ssh-client";
    extraConfig = {
      environment.systemPackages = [
        pkgs.nix
        pkgs.openssh
      ];
    };
  };

  staticSshGatewayModule =
    { ... }:
    {
      imports = [ restrictedIngressGatewayModule ];
      systemd.services.telchar-daemon.wantedBy = pkgs.lib.mkForce [ ];
      systemd.services.telchar-daemon.environment.TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
      systemd.tmpfiles.rules = [
        "d /var/lib/telchar-static-ssh 0700 telchar-ingress telchar -"
      ];
      environment.etc."telchar/telchar.toml".text = ''
        [[backends.static_ssh]]
        name = "builder"
        system = "${pkgs.stdenv.hostPlatform.system}"
        destination = "telchar-builder@builder"
        identity_file = "/var/lib/telchar-static-ssh/identity"
        known_hosts_file = "/var/lib/telchar-static-ssh/known-hosts"
      '';
    };

  restartDatabaseModule = machineModule {
    role = "postgres";
    extraConfig = {
      services.postgresql = {
        enable = true;
        package = pkgs.postgresql;
        enableTCPIP = true;
        ensureDatabases = [ "telchar-recovery" ];
        ensureUsers = [
          {
            name = "telchar-recovery";
            ensureDBOwnership = true;
          }
        ];
        authentication = pkgs.lib.mkForce ''
          local all all trust
          host all all 0.0.0.0/0 trust
          host all all ::/0 trust
        '';
      };
      networking.firewall.enable = false;
    };
  };

  restartDaemonModule =
    { role }:
    machineModule {
      inherit role;
      extraConfig = {
        environment.systemPackages = [ telchar ];
        environment.etc."telchar/telchar.toml".text = ''
          [backends.local]
          name = "local"
          system = "${pkgs.stdenv.hostPlatform.system}"
          maximum_concurrent_builds = 1
        '';
        systemd.services.telchar-recovery-daemon = {
          description = "Telchar restart recovery daemon";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          environment = {
            TELCHAR_DATABASE_URL = "postgresql://telchar-recovery@postgres/telchar-recovery";
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_GATEWAY_STORE_URI = "unix:///nix/var/nix/daemon-socket/socket";
            TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = "/var/lib/telchar-recovery-roots";
            TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
            TELCHAR_SINGLETON_CHECK_INTERVAL_MS = "50";
            NIX_CONFIG = ''
              post-build-hook =
              substituters =
            '';
          };
          serviceConfig = {
            Type = "simple";
            StateDirectory = "telchar-recovery-roots";
            StateDirectoryMode = "0700";
            ExecStart = "${telchar}/bin/telchar daemon --socket /run/telchar-recovery/daemon.sock --frontend-uid 0";
            RuntimeDirectory = "telchar-recovery";
            RuntimeDirectoryMode = "0700";
            Restart = "no";
          };
        };
      };
    };

  collectorModule = machineModule {
    role = "otlp-collector";
    extraConfig = {
      environment.etc."otelcol/config.yaml".text = ''
        receivers:
          otlp:
            protocols:
              grpc:
                endpoint: 0.0.0.0:4317
        exporters:
          file:
            path: /var/lib/telchar-otlp/records.json
        service:
          pipelines:
            traces:
              receivers: [otlp]
              exporters: [file]
            metrics:
              receivers: [otlp]
              exporters: [file]
            logs:
              receivers: [otlp]
              exporters: [file]
      '';
      systemd.services.otelcol = {
        description = "Telchar OTLP integration collector";
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStartPre = "+${pkgs.coreutils}/bin/mkdir -p /var/lib/telchar-otlp";
          ExecStart = "${pkgs.opentelemetry-collector}/bin/otelcol --config /etc/otelcol/config.yaml";
          Restart = "on-failure";
        };
      };
    };
  };
in
rec {
  modules = {
    gateway = gatewayModule;
    stock-client = stockClientModule;
    otlp-collector = collectorModule;
  };

  helpers = {
    waitForTelchar = machine: "${machine}.wait_for_unit(\"telchar.service\")";
    assertNetwork = source: destination: "${source}.succeed(\"ping -c 1 ${destination}\")";
  };

  mkStaticSshFixtureTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        client = staticSshClientModule;
        builder = staticSshBuilderModule { };
      };
      testScript = ''
        start_all()
        builder.wait_for_unit("sshd.service")
        client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar-builder")
        public_key = client.succeed("cat /root/.ssh/telchar-builder.pub").strip()
        builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
        builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
        builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        client.succeed("ssh-keyscan -t ed25519 builder > /root/.ssh/known_hosts 2>/dev/null")
        ssh_options = "-i /root/.ssh/telchar-builder -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts telchar-builder@builder"
        client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar-builder -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts' timeout 30 nix --extra-experimental-features nix-command store ping --store ssh-ng://telchar-builder@builder > /tmp/store-ping 2>&1")
        client.succeed("grep -q 'Store URL: ssh-ng://telchar-builder@builder' /tmp/store-ping || { cat /tmp/store-ping >&2; exit 1; }")
        builder.succeed("grep -Eq '^original_command=.*/?nix-daemon --stdio agent_socket= display=$' /var/lib/telchar-builder/forced-command-evidence || { cat /var/lib/telchar-builder/forced-command-evidence >&2; exit 1; }")
        client.fail("timeout 10 ssh " + ssh_options + " true")
        builder.succeed("grep -q '^original_command=true agent_socket= display=$' /var/lib/telchar-builder/forced-command-evidence")
        client.succeed("test $(timeout -s KILL 5 ssh -tt " + ssh_options + " true >/tmp/pty.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -L 127.0.0.1:22345:127.0.0.1:22 -N " + ssh_options + " >/tmp/local-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -R 127.0.0.1:22346:127.0.0.1:22 -N " + ssh_options + " >/tmp/remote-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("eval $(ssh-agent -s) >/tmp/agent-env && ssh-add /root/.ssh/telchar-builder >/dev/null")
        client.succeed("test $(timeout -s KILL 5 ssh -A " + ssh_options + " true >/tmp/agent-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(DISPLAY=:99 timeout -s KILL 5 ssh -X " + ssh_options + " true >/tmp/x11.out 2>&1; echo $?) -ne 0")
        builder.succeed("! grep -Eq 'agent_socket=[^ ]+|display=[^ ]+' /var/lib/telchar-builder/forced-command-evidence")
        ${testScript}
      '';
    };

  mkStaticSshBuildTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        stock-client = restrictedIngressClientModule;
        gateway = staticSshGatewayModule;
        builder = staticSshBuilderModule { };
      };
      testScript = ''
        start_all()
        builder.wait_for_unit("sshd.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-static-ssh")
        gateway.succeed("sudo -u telchar-ingress ${pkgs.openssh}/bin/ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar-static-ssh/identity")
        public_key = gateway.succeed("cat /var/lib/telchar-static-ssh/identity.pub").strip()
        builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
        builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
        builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        gateway.succeed("${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder > /var/lib/telchar-static-ssh/known-hosts 2>/dev/null && chown telchar-ingress:telchar /var/lib/telchar-static-ssh/known-hosts && chmod 644 /var/lib/telchar-static-ssh/known-hosts")
        gateway.succeed("systemctl start telchar-daemon.service")
        gateway.wait_for_unit("telchar-daemon.service")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        ingress_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + ingress_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ${testScript}
      '';
    };

  mkStaticSshGatewayTest =
    {
      name,
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
            systemd.services.telchar-daemon.wantedBy = pkgs.lib.mkForce [ ];
            systemd.services.telchar-daemon.environment.TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
            systemd.tmpfiles.rules = [
              "d /var/lib/telchar-static-ssh 0700 telchar-ingress telchar -"
            ];
            environment.etc."telchar/telchar.toml".text = ''
              [[backends.static_ssh]]
              name = "primary"
              system = "${pkgs.stdenv.hostPlatform.system}"
              supported_features = ["primary"]
              maximum_concurrent_builds = 1
              destination = "telchar-builder@builder-primary"
              identity_file = "/var/lib/telchar-static-ssh/identity"
              known_hosts_file = "/var/lib/telchar-static-ssh/known-hosts"

              [[backends.static_ssh]]
              name = "secondary"
              system = "${pkgs.stdenv.hostPlatform.system}"
              supported_features = ["secondary"]
              maximum_concurrent_builds = 1
              destination = "telchar-builder@builder-secondary"
              identity_file = "/var/lib/telchar-static-ssh/identity"
              known_hosts_file = "/var/lib/telchar-static-ssh/known-hosts"
            '';
          };
        builder-primary = staticSshBuilderModule {
          role = "static-ssh-builder-primary";
          systemFeatures = [ "primary" ];
        };
        builder-secondary = staticSshBuilderModule {
          role = "static-ssh-builder-secondary";
          systemFeatures = [ "secondary" ];
        };
      };
      testScript = ''
        start_all()
        builder_primary.wait_for_unit("sshd.service")
        builder_secondary.wait_for_unit("sshd.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-static-ssh")
        gateway.succeed("sudo -u telchar-ingress ${pkgs.openssh}/bin/ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar-static-ssh/identity")
        public_key = gateway.succeed("cat /var/lib/telchar-static-ssh/identity.pub").strip()
        for builder in [builder_primary, builder_secondary]:
            builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
            builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
            builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        gateway.succeed("(${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder-primary; ${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder-secondary) > /var/lib/telchar-static-ssh/known-hosts 2>/dev/null && chown telchar-ingress:telchar /var/lib/telchar-static-ssh/known-hosts && chmod 644 /var/lib/telchar-static-ssh/known-hosts")
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
              name = "nomad-primary"
              system = "${pkgs.stdenv.hostPlatform.system}"
              maximum_concurrent_builds = 1
              endpoint = "http://192.168.1.1:4646"
              namespace = "telchar"
              driver = "raw_exec"
              job_name_scope = "telchar-gateway"
              poll_interval_seconds = 1
              runtime_limit_seconds = 120

              transfer_endpoint = "ws://192.168.1.3:7443/callback"

              [backends.nomad.transfer_authentication]
              mode = "hmac"
              key_id = "fixture"
              secret_file = "/var/lib/telchar-import/nomad-transfer.key"

              [backends.nomad.store]
              mode = "daemon"
              uri = "unix:///nix/var/nix/daemon-socket/socket"

              [backends.nomad.transfer_limits]
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

              [backends.nomad.resources]
              cpu_mhz = 100
              memory_mb = 128
              disk_mb = 256

              [backends.nomad.driver_config]
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

  mkRestartRecoveryTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name testScript;
      nodes = {
        postgres = restartDatabaseModule;
        owner = restartDaemonModule { role = "owner"; };
        replacement = restartDaemonModule { role = "replacement"; };
      };
    };

  mkGate3Test =
    {
      name,
      testScript ? "",
    }:
    mkTest {
      inherit name testScript;
      restrictedIngress = true;
      includeCollector = true;
    };

  mkTest =
    {
      name,
      includeCollector ? false,
      restrictedIngress ? false,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name testScript;
      nodes = {
        stock-client = if restrictedIngress then restrictedIngressClientModule else stockClientModule;
        gateway = if restrictedIngress then restrictedIngressGatewayModule else gatewayModule;
      }
      // pkgs.lib.optionalAttrs includeCollector {
        otlp-collector = collectorModule;
      };
    };
}
