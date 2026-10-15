# Experimental sandbox gate, not a production Telchar execution backend.
{ pkgs, worker ? null }:
let
  sandboxExpression = pkgs.writeText "sandbox-proof.nix" ''
    { network }: builtins.derivation {
      # Recover dependency context when evaluating this generated file.
      shell = builtins.storePath "${pkgs.bash}";
      tools = builtins.storePath "${pkgs.coreutils}";
      name = "telchar-microvm-sandbox-proof";
      system = "${pkgs.stdenv.hostPlatform.system}";
      builder = "${pkgs.bash}/bin/bash";
      args = [ "-euc" ${builtins.toJSON ''
        export PATH=${pkgs.coreutils}/bin
        test "$(id -u)" != 0
        test ! -e /run/telchar-microvm-host-only
        test "$(readlink /proc/self/ns/net)" != "$network"
        printf 'sandbox build succeeded\n' > "$out"
      ''} ];
      inherit network;
    }
  '';
  runtime = pkgs.writeText "microvm-runtime.py" (builtins.readFile ../deploy/microvm/runtime.py);
  probe = pkgs.writeShellScript "microvm-sandbox-probe" ''
    set -euo pipefail
    export PATH=${pkgs.lib.makeBinPath [ pkgs.nix pkgs.coreutils ]}
    # No package evaluation/downloads: instantiate a tiny standalone expression
    # against dependencies already registered in the snapshotted store.
    network=$(readlink /proc/self/ns/net)
    drv=$(nix-instantiate ${sandboxExpression} --argstr network "$network" --add-root /run/probe-derivation)
    nix-store --store daemon --realise "$drv" --option substituters "" --add-root /run/probe-output
  '';
  fixture = pkgs.runCommand "microvm-execution-fixture" { } ''
    mkdir -p "$out/tests/microvm" "$out/deploy/microvm"
    cp ${../tests/microvm/bootstrap.py} "$out/tests/microvm/bootstrap.py"
    cp ${../deploy/microvm/runtime.py} "$out/deploy/microvm/runtime.py"
  '';
  adapterCommand = if worker == null then
    "${pkgs.python3}/bin/python3 ${runtime} --probe ${probe}"
  else
    "${pkgs.python3}/bin/python3 ${fixture}/tests/microvm/bootstrap.py --worker ${worker}/bin/microvm-worker --listen 0.0.0.0";
  closure = pkgs.closureInfo {
    rootPaths = [ pkgs.nix pkgs.python3 pkgs.coreutils pkgs.bash pkgs.cacert probe ] ++ pkgs.lib.optional (worker != null) worker;
  };
  entrypoint = pkgs.writeShellScript "microvm-probe-entrypoint" ''
    set -euo pipefail
    export PATH=${pkgs.lib.makeBinPath [ pkgs.nix pkgs.coreutils ]}
    mkdir -p /nix/var/nix/daemon-socket /run /tmp
    chmod 1777 /tmp
    chown root:nixbld /nix/store
    chmod 1775 /nix/store
    nix-store --load-db < ${closure}/registration
    touch /run/telchar-microvm-host-only
    nix --offline daemon &
    daemon=$!
    trap 'kill "$daemon" 2>/dev/null || true' EXIT
    ${adapterCommand} &
    adapter=$!
    trap 'kill "$daemon" "$adapter" 2>/dev/null || true' EXIT
    # Failure of either process fails the container rather than leaving an
    # apparently healthy lifecycle endpoint with a dead Nix daemon.
    wait -n "$daemon" "$adapter"
    exit 1
  '';
  etc = pkgs.runCommand "microvm-probe-etc" { } ''
    mkdir -p "$out/etc/nix"
    echo 'root:x:0:0:root:/root:/bin/sh' > "$out/etc/passwd"
    echo 'root:x:0:' > "$out/etc/group"
    members=""
    for n in $(seq 1 16); do
      echo "nixbld$n:x:$((30000+n)):30000:Nix build user:/var/empty:/bin/sh" >> "$out/etc/passwd"
      members="$members''${members:+,}nixbld$n"
    done
    echo "nixbld:x:30000:$members" >> "$out/etc/group"
    cat > "$out/etc/nix/nix.conf" <<'EOF'
    experimental-features = nix-command
    sandbox = true
    sandbox-fallback = false
    build-users-group = nixbld
    max-jobs = 1
    cores = 1
    substituters =
    EOF
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "telchar-microvm-probe";
  tag = "experimental";
  contents = [ pkgs.nix pkgs.python3 pkgs.coreutils pkgs.bash pkgs.cacert ];
  extraCommands = ''
    mkdir -p ./bin
    ln -s ${entrypoint} ./bin/telchar-microvm-probe
  '';
  fakeRootCommands = ''
    cp -r ${etc}/etc/. ./etc/
    mkdir -p ./tmp ./run ./root ./var/empty
    chmod 1777 ./tmp
  '';
  config = {
    User = "0:0";
    Entrypoint = [ "/bin/telchar-microvm-probe" ];
    Env = [ "HOME=/root" "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" ];
  };
}
