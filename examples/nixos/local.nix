# Composes an operator-managed PostgreSQL database, host store, and OpenSSH ingress.
{ config, pkgs, ... }:
let
  cfg = config.services.telchar;
in
{
  imports = [ ./openssh.nix ];
  services.telchar = {
    enable = true;
    database.url = "host=/run/postgresql user=${cfg.user} dbname=${cfg.user}";
    settings.backends.local = {
      name = "local";
      system = pkgs.stdenv.hostPlatform.system;
      maximum_concurrent_builds = 4;
    };
  };
  services.postgresql = {
    enable = true;
    ensureDatabases = [ cfg.user ];
    ensureUsers = [
      {
        name = cfg.user;
        ensureDBOwnership = true;
      }
    ];
  };
  systemd.services.telchar = {
    after = [
      "postgresql.service"
      "postgresql-setup.service"
    ];
    requires = [
      "postgresql.service"
      "postgresql-setup.service"
    ];
  };
  nix.settings.trusted-users = [ cfg.user ];
  systemd.tmpfiles.rules = [
    "d ${cfg.gatewayStore.gcRootDirectory} 0700 ${cfg.user} ${cfg.group} -"
  ];
}
