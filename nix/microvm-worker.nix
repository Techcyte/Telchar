# Disposable outbound worker image; not exposed as a product backend.
{ pkgs, craneLib, source }:
let
  worker = craneLib.buildPackage {
    pname = "telchar-microvm-fixture";
    version = "2026.9.2";
    src = source;
    cargoExtraArgs = "--locked -p telchar-nomad-worker";
    cargoBuildExtraArgs = "--example microvm-worker";
    doCheck = false;
    TELCHAR_DEFAULT_SSH_PROGRAM = "${pkgs.openssh}/bin/ssh";
    installPhaseCommand = ''
      mkdir -p "$out/bin"
      cp target/release/examples/microvm-worker "$out/bin/microvm-worker"
    '';
  };
in
import ./microvm-probe.nix { inherit pkgs worker; }
