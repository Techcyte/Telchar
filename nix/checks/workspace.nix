# Compile once in a Crane derivation; qualify the complete Cargo suite in a VM.
{
  pkgs,
  craneLib,
  common,
  cargoArtifacts,
}:
let
  artifacts = craneLib.mkCargoDerivation (
    common
    // {
      pname = "telchar-workspace-artifacts";
      inherit cargoArtifacts;
      sourceRoot = "source";
      CARGO_HOME = "/build/cargo-home";
      CARGO_TARGET_DIR = "/build/source/target";
      doCheck = false;
      doInstallCargoArtifacts = false;
      dontFixup = true;
      buildPhaseCargoCommand = ''
        test "$PWD" = /build/source
        cargoWithProfile build --locked --offline -p telchar --bin telchar
        cargoWithProfile test --locked --offline --workspace --exclude telchar-nomad-worker --no-run
        cargoWithProfile test --locked --offline -p telchar-nomad-worker --no-run
      '';
      installPhaseCommand = ''
        mkdir -p "$out/source"
        cp -a ${common.src}/. "$out/source/"
        mv target "$out/target"
        cp -a "$CARGO_HOME" "$out/cargo-home"
        # Cargo fingerprints include the compiler/linker environment. Reuse it in
        # the guest, along with the original absolute source and executable paths.
        export -p > "$out/environment"
      '';
    }
  );
  run = pkgs.writeShellScript "telchar-workspace-tests" ''
    set -euo pipefail
    source ${artifacts}/environment
    export HOME=/home/runner USER=runner LOGNAME=runner TMPDIR=/tmp
    export TELCHAR_TEST_DAEMON_BIN=/build/source/target/debug/telchar
    export CARGO_NET_OFFLINE=true
    cd /build/source
    bash tests/canonical-store.sh true
    bash scripts/test-execution-store.sh ${pkgs.bash}/bin/bash -c true
    bash tests/workspace.sh --locked --offline
  '';
in
pkgs.testers.nixosTest {
  name = "telchar-workspace";
  nodes.machine = {
    virtualisation = {
      cores = 4;
      memorySize = 8192;
      diskSize = 131072;
      writableStore = false;
      additionalPaths = [
        artifacts
        run
      ];
    };
    users.users.runner = {
      isNormalUser = true;
      uid = 1000;
    };
    # Individual fixtures own disposable stores and daemons. No host trust or
    # daemon state participates in this check.
    systemd.services.register-nix-paths.enable = false;
    nix.settings.substituters = pkgs.lib.mkForce [ ];
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("mkdir -p /var/workspace /build /nix/var/nix/daemon-socket && mount --bind /var/workspace /build && mkdir -p /build/source")
    machine.succeed("cp -a ${artifacts}/source/. /build/source/")
    machine.succeed("df -h /build && cp -a ${artifacts}/target /build/source/target", timeout=1200)
    machine.succeed("cp -a ${artifacts}/cargo-home /build/cargo-home")
    machine.succeed("chown -R runner:users /build && chmod -R u+w /build")
    try:
        machine.succeed("${pkgs.bash}/bin/bash -o pipefail -c \"su -s ${pkgs.bash}/bin/bash runner -c '${run}' 2>&1 | tee /tmp/workspace.log /dev/console\"", timeout=2400)
    finally:
        machine.copy_from_machine("/tmp/workspace.log")
  '';
}
