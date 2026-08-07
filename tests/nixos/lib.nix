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
      systemd.services.telchar-daemon = {
        description = "Telchar integration daemon";
        wantedBy = [ "multi-user.target" ];
        environment = {
          OTEL_EXPORTER_OTLP_ENDPOINT = "http://otlp-collector:4317";
          TELCHAR_GATEWAY_STORE_URI = "unix:///nix/var/nix/daemon-socket/socket";
          TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          TELCHAR_SYSTEM = pkgs.stdenv.hostPlatform.system;
          TELCHAR_SUPPORTED_FEATURES = "";
        };
        before = [ "sshd.service" ];
        serviceConfig = {
          User = "telchar-ingress";
          Group = "telchar";
          RuntimeDirectory = "telchar";
          RuntimeDirectoryMode = "0700";
          ExecStart = "${telchar}/bin/telchar daemon --socket /run/telchar/daemon.sock --frontend-uid 995";
        };
      };
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
{
  modules = {
    gateway = gatewayModule;
    stock-client = stockClientModule;
    otlp-collector = collectorModule;
  };

  helpers = {
    waitForTelchar = machine: "${machine}.wait_for_unit(\"telchar.service\")";
    assertNetwork = source: destination: "${source}.succeed(\"ping -c 1 ${destination}\")";
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
