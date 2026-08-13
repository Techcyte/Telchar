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
    version = "0.1.0";
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
      nativeBuildInputs = [ pkgs.postgresql ];
      cargoTestExtraArgs = "--workspace --lib";
    }
  );
}
