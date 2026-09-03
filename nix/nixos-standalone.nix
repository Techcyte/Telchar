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
      port = lib.mkDefault 2222;
    };
  };

  users.users.${config.services.telchar.user}.hashedPassword =
    lib.mkIf config.services.telchar.ingress.openssh.enable "";

  networking.firewall.allowedTCPPorts =
    lib.optional config.services.telchar.ingress.openssh.enable config.services.telchar.ingress.openssh.port
    ++ lib.optional config.services.telchar.callback.openFirewall config.services.telchar.callback.port;
}
