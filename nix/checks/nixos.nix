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
// import ./nixos/local.nix checkArgs
// import ./nixos/nomad.nix checkArgs
// import ./nixos/static-ssh.nix checkArgs
// import ./nixos/recovery.nix checkArgs
// import ./nixos/artifacts.nix checkArgs
