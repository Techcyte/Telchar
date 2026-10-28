# Disposable outbound worker image; not exposed as a product backend.
{ pkgs, craneLib, source }:
let
  worker = craneLib.buildPackage {
    pname = "telchar-microvm-fixture";
    version = "2026.9.2";
    src = source;
    cargoExtraArgs = "--locked -p telchar-nomad-worker";
    cargoBuildCommand = "cargoWithProfile build --example microvm-worker";
    cargoArtifacts = craneLib.buildDepsOnly {
      pname = "telchar-microvm-fixture";
      version = "2026.9.2";
      src = source;
      cargoExtraArgs = "--locked -p telchar-nomad-worker";
      doCheck = false;
      TELCHAR_DEFAULT_SSH_PROGRAM = "${pkgs.openssh}/bin/ssh";
    };
    doCheck = false;
    TELCHAR_DEFAULT_SSH_PROGRAM = "${pkgs.openssh}/bin/ssh";
    installPhaseCommand = ''
      mkdir -p "$out/bin"
      cp target/release/examples/microvm-worker "$out/bin/microvm-worker"
    '';
  };
in
import ./microvm-probe.nix { inherit pkgs worker; }
