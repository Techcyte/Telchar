# Defines reusable service and network defaults for a standalone Telchar gateway.
{
  config,
  lib,
  ...
}:
{
  imports = [ ./nixos-module.nix ];

  services.telchar = {
    enable = true;
    gatewayStore = {
      manageTrustedUser = true;
      manageGcRootDirectory = true;
    };
    ingress.openssh = {
      enable = true;
      port = 2222;
    };
  };

  users.users.${config.services.telchar.user}.hashedPassword =
    lib.mkIf config.services.telchar.ingress.openssh.enable "";

  networking.firewall.allowedTCPPorts = [
    2222
    7443
  ];
}
