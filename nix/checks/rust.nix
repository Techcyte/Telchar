# Defines sandbox-compatible Rust formatting, lint, and library-test checks.
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
  };
  cargoArtifacts = craneLib.buildDepsOnly (
    common
    // {
      doCheck = false;
    }
  );
in
{
  format = craneLib.cargoFmt common;

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
      nativeBuildInputs = [
        pkgs.postgresql
        pkgs.nix
        pkgs.openssh
      ];
      cargoTestExtraArgs = "--workspace --lib";
    }
  );
}
