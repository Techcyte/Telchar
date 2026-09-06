# Composes regular OpenSSH with Telchar's authenticated stdio entry point.
{ config, lib, pkgs, telcharSshCommand, ... }:
let
  cfg = config.services.telchar;
  command = telcharSshCommand {
    inherit pkgs lib;
    inherit (cfg) package socketPath;
  };
in
{
  services.openssh = {
    enable = true;
    extraConfig = ''
      Match User ${cfg.user}
        AuthenticationMethods publickey
        PasswordAuthentication no
        KbdInteractiveAuthentication no
        ForceCommand ${command}
        ExposeAuthInfo yes
        DisableForwarding yes
        PermitTTY no
        PermitUserRC no
        PermitUserEnvironment no
      Match all
    '';
  };
  users.users.${cfg.user} = {
    shell = "${pkgs.bashInteractive}/bin/bash";
    hashedPassword = "*";
  };
}
