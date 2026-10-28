# Dedicated execution image composition; native sandbox and snapshot qualification required.
{ pkgs, worker }:
assert pkgs.stdenv.hostPlatform.system == "aarch64-linux";
let
  runtime = pkgs.runCommand "microvm-execution-runtime" { } ''
    mkdir -p "$out"
    cp ${../deploy/microvm/execution.py} "$out/execution.py"
    cp ${../deploy/microvm/runtime.py} "$out/runtime.py"
  '';
  daemon = pkgs.writeShellScript "microvm-execution-daemon" ''
    exec ${pkgs.nix}/bin/nix --offline daemon
  '';
  ready = pkgs.writeShellScript "microvm-execution-ready" ''
    exec ${pkgs.nix}/bin/nix --offline store ping --store daemon
  '';
  etc = import ./microvm-store.nix { inherit pkgs; };
  closure = pkgs.closureInfo {
    rootPaths = [
      pkgs.nix
      pkgs.python3
      pkgs.coreutils
      pkgs.bash
      worker
      runtime
      daemon
      ready
    ];
  };
  entrypoint = pkgs.writeShellScript "microvm-execution-entrypoint" ''
    set -euo pipefail
    export PATH=${
      pkgs.lib.makeBinPath [
        pkgs.nix
        pkgs.coreutils
      ]
    }
    umask 077
    mkdir -p /nix/var/nix/daemon-socket /run/telchar-microvm /tmp
    chmod 0700 /run/telchar-microvm
    chmod 1777 /tmp
    chown root:nixbld /nix/store
    chmod 1775 /nix/store
    nix-store --load-db < ${closure}/registration
    exec ${pkgs.python3}/bin/python3 ${runtime}/execution.py \
      --listen 0.0.0.0 \
      --daemon ${daemon} --ready ${ready} \
      --worker ${worker}/bin/telchar-microvm-worker \
      --binding /run/telchar-microvm/execution.json \
      --trust /run/telchar-microvm/trust.der --store unix:///nix/var/nix/daemon-socket/socket
  '';
  imageConfig = {
    User = "0:0";
    Entrypoint = [ "/bin/telchar-microvm-execution" ];
    Env = [
      "HOME=/root"
      "PYTHONDONTWRITEBYTECODE=1"
    ];
    Labels = {
      "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
      "org.opencontainers.image.title" = "Telchar MicroVM execution";
    };
  };
in
pkgs.dockerTools.buildLayeredImage {
  name = "telchar-microvm-execution";
  tag = worker.version;
  passthru = { inherit imageConfig; };
  contents = [
    pkgs.nix
    pkgs.python3
    pkgs.coreutils
    pkgs.bash
  ];
  extraCommands = ''
    mkdir -p ./bin
    ln -s ${entrypoint} ./bin/telchar-microvm-execution
  '';
  fakeRootCommands = ''
    cp -r ${etc}/etc/. ./etc/
    mkdir -p ./tmp ./run ./root ./var/empty
    chmod 1777 ./tmp
  '';
  config = imageConfig;
}
