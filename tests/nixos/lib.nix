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
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name testScript;
      nodes = {
        stock-client = stockClientModule;
        gateway = gatewayModule;
      }
      // pkgs.lib.optionalAttrs includeCollector {
        otlp-collector = collectorModule;
      };
    };
}
