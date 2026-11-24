# Trusted Nix builder reachable only from the gateway's private network.
{ pkgs, ... }:
{
  services.openssh.enable = true;
  users.groups.telchar-builder = { };
  users.users.telchar-builder = {
    isSystemUser = true;
    group = "telchar-builder";
    home = "/var/lib/telchar-builder";
    createHome = true;
    shell = "${pkgs.bashInteractive}/bin/bash";
    openssh.authorizedKeys.keys = [
      # Gateway's operator-provided public key.
    ];
  };
  nix.settings.trusted-users = [ "telchar-builder" ];
  services.openssh.extraConfig = ''
    Match User telchar-builder
      AuthenticationMethods publickey
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      ForceCommand ${pkgs.nix}/bin/nix-daemon --stdio
      DisableForwarding yes
      PermitTTY no
      PermitUserRC no
      PermitUserEnvironment no
    Match all
  '';
}
