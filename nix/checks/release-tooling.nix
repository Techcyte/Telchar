# Release scripts inspect maintained non-Cargo files, so use the clean checkout.
{ pkgs }:
pkgs.runCommand "telchar-release-tooling"
  {
    src = pkgs.lib.cleanSource ../..;
    nativeBuildInputs = [
      pkgs.python3
      pkgs.bash
    ];
  }
  ''
    cp -r "$src" source
    chmod -R u+w source
    cd source
    export PYTHONDONTWRITEBYTECODE=1
    python3 scripts/test-prepare-release.py
    python3 scripts/test-release-notes.py
    touch "$out"
  ''
