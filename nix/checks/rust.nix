# Nix owns Rust compilation and results; process integration runs in a VM.
{
  pkgs,
  craneLib,
  source,
}:
let
  common = {
    src = source;
    pname = "telchar";
    version = (builtins.fromTOML (builtins.readFile ../../crates/telchar/Cargo.toml)).package.version;
    CARGO_PROFILE = "dev";
    CARGO_PROFILE_DEV_DEBUG = "0";
    TELCHAR_NIX = "${pkgs.nix}/bin/nix";
    TELCHAR_NIX_BIN = "${pkgs.nix}/bin/nix";
    nativeBuildInputs = [
      pkgs.postgresql
      pkgs.nix
      pkgs.openssh
      pkgs.util-linux
      pkgs.bash
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.cacert
      pkgs.python3
      pkgs.openssl
    ];
  };
  cargoArtifacts = craneLib.buildDepsOnly (
    common
    // {
      cargoExtraArgs = "--locked --workspace";
    }
  );
in
{
  format = craneLib.cargoFmt common;

  release-tooling = import ./release-tooling.nix { inherit pkgs; };

  workspace-tests = import ./workspace.nix {
    inherit
      pkgs
      craneLib
      common
      cargoArtifacts
      ;
  };

  workspace-check = craneLib.mkCargoDerivation (
    common
    // {
      inherit cargoArtifacts;
      buildPhaseCargoCommand = "cargoWithProfile check --locked --workspace";
      doCheck = false;
      doInstallCargoArtifacts = false;
    }
  );

  lint = craneLib.cargoClippy (
    common
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets --all-features -- -D warnings";
    }
  );

  library-tests = craneLib.cargoTest (
    common
    // {
      inherit cargoArtifacts;
      cargoTestExtraArgs = "--workspace --lib";
    }
  );
}
