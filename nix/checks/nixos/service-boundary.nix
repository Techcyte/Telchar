# Evaluates the service module without deployment infrastructure.
{ pkgs, nixosSystem, telcharModule }:
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
        system.stateVersion = "26.05";
      }
    ];
  };
  cfg = evaluated.config;
  options = evaluated.options.services.telchar;
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
pkgs.runCommand "telchar-service-boundary" { } "touch $out"
