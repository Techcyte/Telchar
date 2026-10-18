# Evaluation-only dependency experiment; not an execution-image package set.
{ pkgs, archive, patch }:
let
  library = import ./snapsafe-package.nix { inherit pkgs archive patch; };
  packages = pkgs.extend (_: _: { openssl = library; });
  components = packages.nixVersions.nixComponents_2_34;
  inputs = package: (package.buildInputs or [ ]) ++ (package.propagatedBuildInputs or [ ]);
  consumes = package: dependency:
    builtins.any (input: (input.drvPath or null) == dependency.drvPath) (inputs package);
  describe = package: {
    inherit (package) name drvPath;
    output = package.outPath;
  };
in
assert consumes packages.python3 library;
assert consumes packages.openssh library;
assert consumes packages.curl library;
assert consumes components.nix-util library;
assert consumes components.nix-main library;
assert consumes components.nix-store packages.curl;
{
  openssl = describe library;
  python = describe packages.python3;
  openssh = describe packages.openssh;
  curl = describe packages.curl;
  nix = describe packages.nix;
  nixUtil = describe components.nix-util;
  nixMain = describe components.nix-main;
  nixStore = describe components.nix-store;
}
