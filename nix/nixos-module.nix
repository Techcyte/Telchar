# Defines the Telchar daemon service and application configuration.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.telchar;
  toml = pkgs.formats.toml { };
  protectedDatabase = cfg.database.urlFile != null;
  serviceSettings = lib.recursiveUpdate cfg.settings (
    lib.optionalAttrs protectedDatabase {
      database.url_file = cfg.database.urlFile;
    }
    // lib.optionalAttrs cfg.callback.enable {
      backends.nomad_callback.bind = "${cfg.callback.bindAddress}:${toString cfg.callback.port}";
    }
  );
  configurationFile = toml.generate "telchar.toml" serviceSettings;
  credentialFiles = map (credential: "${credential.name}:${credential.source}") cfg.credentials;
  daemonEnvironment =
    if protectedDatabase then
      removeAttrs cfg.environment [ "TELCHAR_DATABASE_URL" ]
    else
      cfg.environment;
  databaseValidator = pkgs.writeShellScript "validate-telchar-database-url" ''
    exec ${cfg.package}/bin/telchar validate-database-tls \
      ${lib.escapeShellArg cfg.database.urlFile} \
      ${lib.escapeShellArg cfg.database.rootCertificateFile}
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
      description = "System user owning the Telchar daemon.";
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
      url = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "PostgreSQL connection URL used by Telchar.";
      };
      urlFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected file containing the external PostgreSQL connection URL.";
      };
      rootCertificateFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected CA certificate required by the external PostgreSQL URL.";
      };
    };

    nomad = {
      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Protected Nomad token source mounted read-only into the Telchar service.";
      };
    };

    callback = {
      enable = lib.mkEnableOption "the authenticated Nomad callback listener";
      bindAddress = lib.mkOption {
        type = lib.types.str;
        default = "0.0.0.0";
        description = "Address for the authenticated Nomad callback listener.";
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 7443;
        description = "TCP port for the authenticated Nomad callback listener.";
      };
      openFirewall = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Whether to open the callback listener port in the host firewall.";
      };
    };

    gatewayStore = {
      uri = lib.mkOption {
        type = lib.types.str;
        default = "unix:///nix/var/nix/daemon-socket/socket";
        description = "Gateway store URI used for closure and output transfer.";
      };
      directory = lib.mkOption {
        type = lib.types.str;
        default = "/nix/store";
        description = "Physical local store directory for best-effort capacity measurement; inaccessible paths warn without blocking admission.";
      };
      gcRootDirectory = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/telchar/gc-roots";
        description = "Directory holding retained gateway-store GC roots.";
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
        assertion = lib.all (
          credential:
          lib.hasPrefix "/" credential.source && !(lib.hasPrefix builtins.storeDir credential.source)
        ) cfg.credentials;
        message = "services.telchar.credentials sources must be absolute and outside the Nix store";
      }
      {
        assertion =
          !protectedDatabase
          || (
            lib.hasPrefix "/" cfg.database.urlFile && !(lib.hasPrefix builtins.storeDir cfg.database.urlFile)

          );
        message = "services.telchar.database protected files must be absolute and outside the Nix store";
      }
      {
        assertion =
          cfg.database.rootCertificateFile == null
          || (
            protectedDatabase
            && lib.hasPrefix "/" cfg.database.rootCertificateFile
            && !(lib.hasPrefix builtins.storeDir cfg.database.rootCertificateFile)
          );
        message = "services.telchar.database.rootCertificateFile requires a protected URL file and an absolute CA path outside the Nix store";
      }
      {
        assertion =
          cfg.nomad.tokenFile == null
          || (
            lib.hasPrefix "/" cfg.nomad.tokenFile && !(lib.hasPrefix builtins.storeDir cfg.nomad.tokenFile)
          );
        message = "services.telchar.nomad.tokenFile must be absolute and outside the Nix store";
      }
    ];

    users.groups.${cfg.group} = { };
    users.users.${cfg.user} = {
      isSystemUser = true;
      uid = cfg.frontendUid;
      group = cfg.group;
      home = "/var/lib/telchar";
      createHome = true;
    };

    environment.systemPackages = [ cfg.package ] ++ cfg.backendPackages;

    systemd.services.telchar = {
      description = "Telchar Nix build gateway";
      wantedBy = [ "multi-user.target" ];
      after = [
        "network-online.target"
      ];
      wants = [ "network-online.target" ];
      environment = {
        TELCHAR_CONFIG = configurationFile;
        TELCHAR_GATEWAY_STORE_URI = cfg.gatewayStore.uri;
        TELCHAR_GATEWAY_STORE_DIRECTORY = cfg.gatewayStore.directory;
        TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = cfg.gatewayStore.gcRootDirectory;
        TMPDIR = "/var/lib/telchar/import";
      }
      // lib.optionalAttrs (!protectedDatabase && cfg.database.url != null) {
        TELCHAR_DATABASE_URL = cfg.database.url;
      }
      // daemonEnvironment;
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
        ExecStartPre = lib.optional (cfg.database.rootCertificateFile != null) databaseValidator;
        Restart = "on-failure";
        RestartSec = "5s";
        LoadCredential = credentialFiles;
        BindReadOnlyPaths = lib.optional (
          cfg.nomad.tokenFile != null
        ) "${dirOf cfg.nomad.tokenFile}:/run/telchar/credentials";
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/telchar/import 0700 ${cfg.user} ${cfg.group} -"
    ];
  };
}
