# AWS image-build context; runtime and restored-snapshot qualification required.
{ pkgs, worker, trust }:
assert pkgs.stdenv.hostPlatform.system == "aarch64-linux";
let
  policy = import ./microvm-store.nix { inherit pkgs; };
  daemon = pkgs.writeShellScript "microvm-daemon" ''
    exec ${pkgs.nix}/bin/nix --offline daemon
  '';
  ready = pkgs.writeShellScript "microvm-ready" ''
    exec ${pkgs.nix}/bin/nix --offline store ping --store daemon
  '';
  closure = pkgs.closureInfo { rootPaths = [ pkgs.nix worker trust daemon ready ]; };
  entrypoint = pkgs.writeText "microvm-entrypoint" ''
    #!/bin/sh
    set -eu
    umask 077
    mkdir -p /nix/var/nix/daemon-socket /run/telchar-microvm /tmp
    chmod 0700 /run/telchar-microvm
    chmod 1777 /tmp
    chown root:nixbld /nix/store
    chmod 1775 /nix/store
    ${pkgs.nix}/bin/nix-store --load-db < /opt/telchar/registration
    exec /usr/bin/python3.12 /opt/telchar/execution.py \
      --listen 0.0.0.0 --daemon ${daemon} --ready ${ready} \
      --worker ${worker}/bin/telchar-microvm-worker \
      --binding /run/telchar-microvm/execution.json \
      --trust ${trust} --store unix:///nix/var/nix/daemon-socket/socket
  '';
in
pkgs.runCommand "microvm-aws-context" { } ''
  mkdir -p "$out/nix/store" "$out/runtime"
  while read -r path; do
    cp -a "$path" "$out/nix/store/"
  done < ${closure}/store-paths
  cp ${closure}/registration "$out/runtime/registration"
  cp ${../deploy/microvm/execution.py} "$out/runtime/execution.py"
  cp ${../deploy/microvm/runtime.py} "$out/runtime/runtime.py"
  cp ${entrypoint} "$out/runtime/entrypoint"
  cp ${policy}/etc/nix/nix.conf "$out/runtime/nix.conf"
  cp ${../deploy/microvm/Dockerfile} "$out/Dockerfile"
''
