# Defines NixOS VM integration checks for module, ingress, backend, recovery, and artifact behavior.
{
  pkgs,
  nixosSystem,
  system,
  telchar,
  nomadWorker,
  telcharImage,
  nomadWorkerImage,
  telcharModule,
  standaloneModule,
  mkStandaloneSystem,
}:
let
  checkArgs = {
    inherit
      pkgs
      system
      telchar
      nomadWorker
      telcharModule
      ;
  };
in
import ./nixos/oci.nix {
  inherit
    pkgs
    system
    telcharImage
    nomadWorkerImage
    ;
}
// {
  nixos-export-connections = import ./nixos/export-connections.nix checkArgs;
  nixos-import-connections = import ./nixos/import-connections.nix checkArgs;
  nixos-query-trace = import ./nixos/import-connections.nix (checkArgs // { traceQueries = true; });
}
// import ./nixos/module.nix checkArgs
// import ./nixos/standalone.nix {
  inherit
    pkgs
    nixosSystem
    standaloneModule
    mkStandaloneSystem
    ;
}
// import ./nixos/local.nix checkArgs
// removeAttrs (import ./nixos/nomad.nix checkArgs) [ "nixos-nomad-fixture" ]
// {
  nixos-nomad-polling = import ./nixos/nomad-polling.nix checkArgs;
  nixos-nomad-credential = import ./nixos/nomad-credential.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
  nixos-nomad-credential-renewal = import ./nixos/nomad-credential-renewal.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
}
// removeAttrs (import ./nixos/static-ssh.nix checkArgs) [ "nixos-static-ssh-fixture" ]
// {
  nixos-ssh-modes = import ./nixos/ssh-modes.nix {
    inherit pkgs nixosSystem telcharModule;
  };
  nixos-ssh-ca-authentication = import ./nixos/ssh-ca-authentication.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
  nixos-ssh-host-certificate-renewal = import ./nixos/ssh-host-certificate-renewal.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
  nixos-vault-options = import ./nixos/vault-options.nix {
    inherit pkgs nixosSystem telcharModule;
  };
  nixos-vault-aws-auth = import ./nixos/vault-aws-auth.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
  nixos-restart-reboot = import ./nixos/restart-reboot.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
}
// import ./nixos/recovery.nix checkArgs
