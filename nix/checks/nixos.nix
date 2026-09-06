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
  exampleModule,
}:
let
  checkArgs = {
    inherit pkgs system telchar nomadWorker;
    telcharModule = exampleModule;
  };
  standaloneModule = {
    imports = [ exampleModule ../../examples/nixos/standalone.nix ];
  };
in
import ./nixos/oci.nix {
  inherit pkgs system telcharImage nomadWorkerImage;
}
// {
  nixos-service-boundary = import ./nixos/service-boundary.nix {
    inherit pkgs nixosSystem telcharModule;
  };
  nixos-export-connections = import ./nixos/export-connections.nix checkArgs;
  nixos-import-connections = import ./nixos/import-connections.nix checkArgs;
  nixos-query-trace = import ./nixos/import-connections.nix (checkArgs // { traceQueries = true; });
}
// import ./nixos/module.nix checkArgs
// import ./nixos/local.nix checkArgs
// removeAttrs (import ./nixos/nomad.nix checkArgs) [ "nixos-nomad-fixture" ]
// {
  nixos-nomad-polling = import ./nixos/nomad-polling.nix checkArgs;
  nixos-worker-telemetry = import ./nixos/worker-telemetry.nix checkArgs;
  nixos-nomad-credential = import ./nixos/nomad-credential.nix {
    inherit pkgs system telchar standaloneModule;
  };
  nixos-nomad-credential-reload = import ./nixos/nomad-credential-reload.nix {
    inherit pkgs system telchar standaloneModule;
  };
}
// removeAttrs (import ./nixos/static-ssh.nix checkArgs) [ "nixos-static-ssh-fixture" ]
// import ./nixos/recovery.nix checkArgs
