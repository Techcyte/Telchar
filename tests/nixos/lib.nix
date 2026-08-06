{ pkgs, telchar }:
let
  machineModule = { role, extraConfig ? {} }:
    { ... }:
    {
      networking.firewall.enable = false;
      system.stateVersion = "26.05";
      environment.etc."telchar-test-role".text = role;
    }
    // extraConfig;

  gatewayModule = machineModule {
    role = "gateway";
    extraConfig = {
      systemd.services.telchar = {
        description = "Telchar integration service";
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${telchar}/bin/telchar";
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
    assertNetwork = source: destination:
      "${source}.succeed(\"ping -c 1 ${destination}\")";
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
