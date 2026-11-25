# Compile once in a Crane derivation; qualify the complete Cargo suite in a VM.
{
  pkgs,
  craneLib,
  common,
  cargoArtifacts,
  runtimeSource,
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
    export PATH="/run/wrappers/bin:$PATH"
    export HOME=/home/runner USER=runner LOGNAME=runner TMPDIR=/tmp
    export TELCHAR_TEST_DAEMON_BIN=/build/source/target/debug/telchar
    export CARGO_NET_OFFLINE=true
    cd /build/source
    bash tests/canonical-store.sh true
    bash scripts/test-execution-store.sh ${pkgs.bash}/bin/bash -c true
    export TELCHAR_TEST_SOURCE_STORE='local?read-only=true'
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
        runtimeSource
      ];
    };
    users.users.runner = {
      isNormalUser = true;
      uid = 1000;
    };
    systemd.services.register-nix-paths.wantedBy = pkgs.lib.mkForce [ ];
    # The registered VM closure is a read-only bootstrap source for private
    # fixture stores. Tests do not use the VM daemon for execution.
    systemd.services.nix-daemon.enable = false;
    systemd.sockets.nix-daemon.enable = false;
    nix.settings.substituters = pkgs.lib.mkForce [ ];
    nix.settings.build-users-group = pkgs.lib.mkForce "";
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    # Nested private fixture stores need a bootstrap filesystem without overlays.
    machine.succeed("set -o pipefail; mkdir -p /var/bootstrap-store && reg=$(grep -o 'regInfo=[^ ]*' /proc/cmdline) && grep '^/nix/store/' \"''${reg#regInfo=}\" | sort -u | xargs -r -P 4 -I '{}' cp -a '{}' /var/bootstrap-store/ && cp -a \"$(dirname \"''${reg#regInfo=}\")\" /var/bootstrap-store/", timeout=1200)
    machine.succeed("mount --bind /var/bootstrap-store /nix/store")
    machine.succeed("systemctl start register-nix-paths.service")
    machine.succeed("mkdir -p /var/workspace /build /nix/var/nix/daemon-socket && mount --bind /var/workspace /build && mkdir -p /build/source")
    machine.succeed("cp -a ${runtimeSource}/. /build/source/")
    machine.succeed("df -h /build && cp -a ${artifacts}/target /build/source/target", timeout=1200)
    machine.succeed("cp -a ${artifacts}/cargo-home /build/cargo-home")
    machine.succeed("chown -R runner:users /build && chmod -R u+w /build")
    machine.succeed("mount -t tracefs tracefs /sys/kernel/tracing || test -d /sys/kernel/tracing/events/signal; echo 'sig == 9' > /sys/kernel/tracing/events/signal/signal_generate/filter; echo 16384 > /sys/kernel/tracing/buffer_size_kb; echo 1 > /sys/kernel/tracing/events/signal/signal_generate/enable; echo 1 > /sys/kernel/tracing/events/sched/sched_process_exec/enable; echo 1 > /sys/kernel/tracing/events/sched/sched_process_fork/enable; echo 1 > /sys/kernel/tracing/tracing_on")
    try:
        machine.succeed("${pkgs.bash}/bin/bash -o pipefail -c \"su -s ${pkgs.bash}/bin/bash runner -c '${run}' 2>&1 | tee /tmp/workspace.log /dev/console\"", timeout=2400)
    finally:
        machine.succeed("cat /sys/kernel/tracing/trace > /tmp/signals.log; journalctl -k --no-pager > /tmp/kernel.log; journalctl -u systemd-oomd --no-pager > /tmp/oomd.log")
        machine.succeed("find /tmp -maxdepth 3 -name daemon.log -exec sh -c 'echo DAEMON_LOG=$1; cat \"$1\"' sh {} \\; > /tmp/daemons.log")
        machine.copy_from_machine("/tmp/signals.log")
        machine.copy_from_machine("/tmp/daemons.log")
        machine.copy_from_machine("/tmp/kernel.log")
        machine.copy_from_machine("/tmp/oomd.log")
        machine.copy_from_machine("/tmp/workspace.log")
  '';
}
