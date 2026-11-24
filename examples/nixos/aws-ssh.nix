# Gateway composition for an operator-provisioned NixOS EC2 instance.
{ config, pkgs, ... }:
let
  cfg = config.services.telchar;
in
{
  imports = [ ./openssh.nix ];

  services.telchar = {
    enable = true;
    database.url = "host=/run/postgresql user=${cfg.user} dbname=${cfg.user}";
    settings.backends.ssh = [
      {
        system = "x86_64-linux";
        maximum_concurrent_builds = 2;
        ssh_user = "telchar-builder";
        identity_file = "/var/lib/telchar/ssh/id_ed25519";
        known_hosts_file = "/var/lib/telchar/ssh/known_hosts";
        builders = {
          source = "static";
          worker.address = "builder.internal.example.com";
        };
      }
    ];
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
    "d /var/lib/telchar/ssh 0700 ${cfg.user} ${cfg.group} -"
  ];
}
