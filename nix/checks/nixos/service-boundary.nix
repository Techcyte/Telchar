# Evaluates the service module without deployment infrastructure.
{
  pkgs,
  nixosSystem,
  telcharModule,
}:
let
  evaluated = nixosSystem {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [
      telcharModule
      {
        services.telchar = {
          enable = true;
          package = pkgs.hello;
          settings.database.url = "host=/run/operator-postgresql user=telchar dbname=builds";
        };
        boot.loader.grub.enable = false;
        fileSystems."/" = {
          device = "/dev/vda";
          fsType = "ext4";
        };
        system.stateVersion = "26.05";
      }
    ];
  };
  cfg = evaluated.config;
  options = evaluated.options.services.telchar;
  administrative =
    settings:
    (evaluated.extendModules {
      modules = [ { services.telchar = settings; } ];
    }).config;
  enabled = administrative { adminSocketPath = "/run/telchar/admin.sock"; };
  accepts = settings: builtins.all (entry: entry.assertion) (administrative settings).assertions;
in
assert !(options ? ingress);
assert !(options ? sshHostCertificateRenewal);
assert !(options ? vaultAwsAuth);
assert !(options.database ? manage);
assert !(options.nomad ? renewal);
assert !(options.gatewayStore ? manageTrustedUser);
assert !(options.gatewayStore ? manageGcRootDirectory);
assert !cfg.services.postgresql.enable;
assert !cfg.services.openssh.enable;
assert !(cfg.systemd.services ? telchar-sshd);
assert !(cfg.systemd.services.telchar.environment ? TELCHAR_DATABASE_URL);
assert cfg.systemd.services.telchar.serviceConfig.User == "telchar";
assert cfg.systemd.services.telchar.serviceConfig.StateDirectory == "telchar";
assert !(cfg.systemd.services.telchar.environment ? TELCHAR_ADMIN_SOCKET);
assert
  enabled.systemd.services.telchar.environment.TELCHAR_ADMIN_SOCKET == "/run/telchar/admin.sock";
assert enabled.systemd.services.telchar.serviceConfig.User == "telchar";
assert enabled.systemd.services.telchar.serviceConfig.RuntimeDirectoryMode == "0700";
assert accepts { adminSocketPath = "/run/telchar/admin.sock"; };
assert !(accepts { adminSocketPath = "/run/telchar/daemon.sock"; });
assert !(accepts { adminSocketPath = "/tmp/admin.sock"; });
assert !(accepts { adminSocketPath = "/run/telchar/../admin.sock"; });
assert
  !(accepts {
    adminSocketPath = "/run/telchar/admin.sock";
    environment.TELCHAR_ADMIN_SOCKET = "/run/telchar/other.sock";
  });
pkgs.runCommand "telchar-service-boundary" { } "touch $out"
