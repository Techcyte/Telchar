# Defines the opinionated NixOS service boundary for Telchar, PostgreSQL, gateway-store access, and restricted SSH ingress.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.telchar;
  toml = pkgs.formats.toml { };
  configurationFile = toml.generate "telchar.toml" cfg.settings;
  credentialFiles = map (credential: "${credential.name}:${credential.source}") cfg.credentials;
  forcedCommand = pkgs.writeShellScript "telchar-forced-command" ''
    set -eu
    fingerprint="$(${pkgs.openssh}/bin/ssh-keygen -lf ${lib.escapeShellArg cfg.openssh.authorizedKeysFile} | ${pkgs.gawk}/bin/awk 'NR == 1 { print $2 }')"
    exec env \
      TELCHAR_IPC_SOCKET=${lib.escapeShellArg cfg.socketPath} \
      TELCHAR_AUTHENTICATED_KEY="$fingerprint" \
      ${cfg.package}/bin/telchar serve-stdio
  '';
in
{
  options.services.telchar = {
    enable = lib.mkEnableOption "Telchar Nix build gateway";

    package = lib.mkOption {
      type = lib.types.package;
      description = "Telchar package to run.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "telchar";
      description = "System user owning the Telchar daemon and ingress.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "telchar";
      description = "System group owning Telchar state.";
    };

    frontendUid = lib.mkOption {
      type = lib.types.int;
      default = 995;
      description = "UID authorized to connect to the private frontend socket.";
    };

    socketPath = lib.mkOption {
      type = lib.types.str;
      default = "/run/telchar/daemon.sock";
      description = "Private daemon frontend socket.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      description = "Strict Telchar TOML configuration.";
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      description = "Additional operator-controlled daemon environment.";
    };

    credentials = lib.mkOption {
      type = lib.types.listOf (
        lib.types.submodule {
          options = {
            name = lib.mkOption {
              type = lib.types.strMatching "[A-Za-z0-9_.-]+";
              description = "Credential name exposed below CREDENTIALS_DIRECTORY.";
            };
            source = lib.mkOption {
              type = lib.types.str;
              description = "Absolute protected credential source file outside the Nix store.";
            };
          };
        }
      );
      default = [ ];
      description = "Files loaded through systemd credentials rather than the Nix store.";
    };

    backendPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "Operator-selected backend helper packages available to the daemon.";
    };

    database = {
      enable = lib.mkEnableOption "local PostgreSQL coordination" // {
        default = true;
      };
      name = lib.mkOption {
        type = lib.types.str;
        default = "telchar";
        description = "Local PostgreSQL database name.";
      };
      url = lib.mkOption {
        type = lib.types.str;
        default = "postgresql://${cfg.user}@/${cfg.database.name}?host=/run/postgresql";
        defaultText = lib.literalExpression ''"postgresql://\${config.services.telchar.user}@/\${config.services.telchar.database.name}?host=/run/postgresql"'';
        description = "PostgreSQL connection URL used by Telchar.";
      };
    };

    gatewayStore = {
      enable = lib.mkEnableOption "trusted gateway Nix daemon access" // {
        default = true;
      };
      uri = lib.mkOption {
        type = lib.types.str;
        default = "unix:///nix/var/nix/daemon-socket/socket";
        description = "Gateway store URI used for closure and output transfer.";
      };
      gcRootDirectory = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar/gc-roots";
        description = "Directory holding retained gateway-store GC roots.";
      };
    };

    openssh = {
      enable = lib.mkEnableOption "restricted stock-Nix OpenSSH ingress" // {
        default = true;
      };
      authorizedKeysFile = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar/.ssh/authorized_keys";
        description = "Operator-managed authorized keys file for Telchar ingress.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.hasPrefix "/run/" cfg.socketPath;
        message = "services.telchar.socketPath must be below /run";
      }
      {
        assertion = cfg.openssh.enable -> lib.hasPrefix "/" cfg.openssh.authorizedKeysFile;
        message = "services.telchar.openssh.authorizedKeysFile must be absolute";
      }
      {
        assertion = lib.all (
          credential:
          lib.hasPrefix "/" credential.source && !(lib.hasPrefix builtins.storeDir credential.source)
        ) cfg.credentials;
        message = "services.telchar.credentials sources must be absolute and outside the Nix store";
      }
    ];

    users.groups.${cfg.group} = { };
    users.users.${cfg.user} = {
      isSystemUser = true;
      uid = cfg.frontendUid;
      group = cfg.group;
      home = "/var/lib/telchar";
      createHome = true;
      shell = "${pkgs.bashInteractive}/bin/bash";
    };

    environment.systemPackages = [ cfg.package ] ++ cfg.backendPackages;

    services.postgresql = lib.mkIf cfg.database.enable {
      enable = true;
      ensureDatabases = [ cfg.database.name ];
      ensureUsers = [
        {
          name = cfg.user;
          ensureDBOwnership = true;
        }
      ];
    };

    nix.settings.trusted-users = lib.mkIf cfg.gatewayStore.enable [ cfg.user ];

    services.openssh = lib.mkIf cfg.openssh.enable {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitTTY = false;
        AllowTcpForwarding = false;
        AllowAgentForwarding = false;
        X11Forwarding = false;
        PermitUserEnvironment = false;
      };
      extraConfig = ''
        Match User ${cfg.user}
          AuthorizedKeysFile ${cfg.openssh.authorizedKeysFile}
          ForceCommand ${forcedCommand}
          DisableForwarding yes
          PermitTTY no
      '';
    };

    systemd.services.telchar = {
      description = "Telchar Nix build gateway";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ] ++ lib.optional cfg.database.enable "postgresql.service";
      wants = [ "network-online.target" ];
      requires = lib.optional cfg.database.enable "postgresql.service";
      environment = {
        TELCHAR_CONFIG = configurationFile;
        TELCHAR_DATABASE_URL = cfg.database.url;
        TELCHAR_GATEWAY_STORE_URI = cfg.gatewayStore.uri;
        TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = cfg.gatewayStore.gcRootDirectory;
        TMPDIR = "/var/lib/telchar/import";
      }
      // cfg.environment;
      path = [
        pkgs.nix
        pkgs.openssh
      ]
      ++ cfg.backendPackages;
      serviceConfig = {
        User = cfg.user;
        Group = cfg.group;
        RuntimeDirectory = "telchar";
        RuntimeDirectoryMode = "0700";
        StateDirectory = "telchar";
        StateDirectoryMode = "0700";
        ExecStart = "${cfg.package}/bin/telchar daemon --socket ${cfg.socketPath} --frontend-uid ${toString cfg.frontendUid}";
        Restart = "on-failure";
        LoadCredential = credentialFiles;
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/telchar/import 0700 ${cfg.user} ${cfg.group} -"
      "d ${cfg.gatewayStore.gcRootDirectory} 0700 ${cfg.user} ${cfg.group} -"
    ];
  };
}
