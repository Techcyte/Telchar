# Real PostgreSQL and gateway stores behind restricted SSH, with stock Nix or Lix clients.
{
  pkgs,
  telchar,
  common,
}:
let
  inherit (common)
    machineModule
    gatewayModule
    stockClientModule
    collectorModule
    ;
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
          "postgresql-setup.service"
        ];
        wants = [ "network-online.target" ];
        requires = [
          "postgresql.service"
          "postgresql-setup.service"
        ];
        environment = {
          OTEL_EXPORTER_OTLP_ENDPOINT = "http://otlp-collector:4317";
          TELCHAR_DATABASE_URL = "host=/run/postgresql user=telchar-ingress dbname=telchar-ingress";
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
          } > /tmp/telchar-forced-command-evidence
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

  lixRestrictedIngressClientModule = machineModule {
    role = "lix-client";
    extraConfig = {
      nix.package = pkgs.lix;
      environment.systemPackages = [
        pkgs.lix
        pkgs.openssh
      ];
    };
  };

in
rec {
  inherit restrictedIngressGatewayModule restrictedIngressClientModule;

  # Exercises the pinned Lix client against the same Nix gateway used by stock
  # clients; compatibility assertions belong to the supplied workload script.
  mkLixRestrictedIngressTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name testScript;
      nodes = {
        stock-client = lixRestrictedIngressClientModule;
        gateway = restrictedIngressGatewayModule;
        otlp-collector = collectorModule;
      };
    };

  # Full daemon ingress for protocol, output-retention, and local-build contracts,
  # with separate client/store authority and captured OTLP evidence.
  mkRestrictedIngressTest =
    {
      name,
      testScript ? "",
    }:
    mkTest {
      inherit name testScript;
      restrictedIngress = true;
      includeCollector = true;
    };

  # The default gateway runs the CLI smoke path, not a build daemon. Restricted
  # ingress selects the real daemon; the caller supplies all scenario assertions.
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
