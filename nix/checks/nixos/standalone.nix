# Verifies the public standalone profile and complete-system composition contract.
{
  pkgs,
  nixosSystem,
  standaloneModule,
  mkStandaloneSystem,
}:
let
  system = mkStandaloneSystem {
    inherit (pkgs.stdenv.hostPlatform) system;
    modules = [
      {
        boot.loader.grub.devices = [ "/dev/vda" ];
        fileSystems."/" = {
          device = "/dev/disk/by-label/nixos";
          fsType = "ext4";
        };
        system.stateVersion = "26.05";
      }
    ];
  };
  standalone = system.config;
  customPorts = mkStandaloneSystem {
    inherit (pkgs.stdenv.hostPlatform) system;
    modules = [
      {
        services.telchar = {
          ingress.openssh.port = 2200;
          callback = {
            enable = true;
            port = 7444;
            openFirewall = true;
          };
        };
        system.stateVersion = "26.05";
      }
    ];
  };
  protectedSystem = mkStandaloneSystem {
    inherit (pkgs.stdenv.hostPlatform) system;
    modules = [
      {
        services.telchar = {
          database = {
            urlFile = "/var/lib/telchar/credentials/database-url";
            rootCertificateFile = "/var/lib/telchar/credentials/database-ca.pem";
          };
          environment.TELCHAR_DATABASE_URL = "postgresql://embedded-secret@database/telchar?sslmode=disable";
        };
        system.stateVersion = "26.05";
      }
    ];
  };
  direct = (nixosSystem {
    inherit (pkgs.stdenv.hostPlatform) system;
    modules = [
      standaloneModule
      { system.stateVersion = "26.05"; }
    ];
  }).config;
in
{
  nixos-standalone-profile = pkgs.runCommand "telchar-nixos-standalone-profile" { } ''
    test ${if direct.services.telchar.enable then "1" else "0"} = 1
    test ${if standalone.services.telchar.enable then "1" else "0"} = 1
    test ${if standalone.services.telchar.gatewayStore.manageTrustedUser then "1" else "0"} = 1
    test ${if standalone.services.telchar.gatewayStore.manageGcRootDirectory then "1" else "0"} = 1
    test ${if standalone.services.telchar.ingress.openssh.enable then "1" else "0"} = 1
    test ${toString standalone.services.telchar.ingress.openssh.port} = 2222
    test ${if builtins.elem 2222 standalone.networking.firewall.allowedTCPPorts then "1" else "0"} = 1
    test ${if builtins.elem 7443 standalone.networking.firewall.allowedTCPPorts then "1" else "0"} = 0
    test ${if standalone.services.telchar.callback.enable then "1" else "0"} = 0
    test ${if standalone.services.telchar.callback.openFirewall then "1" else "0"} = 0
    test ${if builtins.elem 2200 customPorts.config.networking.firewall.allowedTCPPorts then "1" else "0"} = 1
    test ${if builtins.elem 7444 customPorts.config.networking.firewall.allowedTCPPorts then "1" else "0"} = 1
    test ${if builtins.elem 2222 customPorts.config.networking.firewall.allowedTCPPorts then "1" else "0"} = 0
    test ${if builtins.elem 7443 customPorts.config.networking.firewall.allowedTCPPorts then "1" else "0"} = 0
    grep -q 'bind = "0.0.0.0:7444"' ${customPorts.config.systemd.services.telchar.environment.TELCHAR_CONFIG}
    grep -q '\[backends.nomad_callback\]' ${customPorts.config.systemd.services.telchar.environment.TELCHAR_CONFIG}
    test ${if protectedSystem.config.systemd.services.telchar.environment ? TELCHAR_DATABASE_URL then "1" else "0"} = 0
    touch $out
  '';
}
