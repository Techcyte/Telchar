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
// import ./nixos/module.nix checkArgs
// import ./nixos/standalone.nix {
  inherit
    pkgs
    nixosSystem
    standaloneModule
    mkStandaloneSystem
    ;
}
// {
  nixos-postgresql-tls = import ./nixos/postgresql-tls.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
}
// import ./nixos/local.nix checkArgs
// import ./nixos/nomad.nix checkArgs
// {
  nixos-nomad-credential = import ./nixos/nomad-credential.nix {
    inherit
      pkgs
      system
      telchar
      standaloneModule
      ;
  };
}
// import ./nixos/static-ssh.nix checkArgs
// {
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
}
// import ./nixos/recovery.nix checkArgs
// import ./nixos/artifacts.nix checkArgs
