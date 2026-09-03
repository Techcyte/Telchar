# Verifies authenticated Nomad callbacks succeed and unauthenticated callbacks fail closed.
{
  pkgs,
  telchar,
  nomadWorker,
}:
let
  harness = import ../../../tests/nixos/lib.nix {
    inherit pkgs telchar;
  };
  callbackProbe = pkgs.writeText "callback-probe.py" ''
    import base64
    import os
    import socket
    import sys

    mode = sys.argv[1]
    connection = socket.create_connection(("gateway", 7443), timeout=5)
    key = base64.b64encode(os.urandom(16)).decode()
    connection.sendall(
        (
            "GET /callback HTTP/1.1\r\n"
            "Host: gateway\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "Sec-WebSocket-Protocol: telchar-nomad-transfer-v1\r\n\r\n"
        ).encode()
    )
    response = connection.recv(4096)
    if b" 101 " not in response:
        raise SystemExit(f"WebSocket upgrade rejected: {response!r}")

    if mode == "invalid":
        payload = b"not-a-telchar-authentication-frame"
        mask = os.urandom(4)
        header = bytes([0x82, 0x80 | len(payload)]) + mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        connection.sendall(header + masked)
    elif mode != "missing":
        raise SystemExit(f"unknown mode: {mode}")

    connection.settimeout(15)
    try:
        data = connection.recv(1)
    except ConnectionResetError:
        data = b""
    if data:
        raise SystemExit(f"unauthenticated callback remained open: {data!r}")
  '';
  callbackDerivation = pkgs.writeText "callback-authentication-build.nix" ''
    derivation {
      name = "callback-authentication-build";
      system = builtins.currentSystem;
      builder = builtins.storePath "${pkgs.runtimeShell}";
      args = [ "-c" "printf callback-authenticated > $out" ];
    }
  '';
in
harness.mkNomadGatewayTest {
  name = "telchar-nixos-callback-authentication";
  worker = nomadWorker;
  testScript = ''
    stock_client.succeed("${pkgs.python3}/bin/python3 ${callbackProbe} invalid")
    stock_client.succeed("${pkgs.python3}/bin/python3 ${callbackProbe} missing")

    stock_client.succeed("cp ${callbackDerivation} /tmp/callback-authentication-build.nix")
    derivation_path = stock_client.succeed("nix-instantiate /tmp/callback-authentication-build.nix").strip()
    derivation_export = stock_client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
    gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
    build = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' '" + derivation_path + "^*'"
    stock_client.succeed(build + " > /tmp/callback-authentication-build.out 2>&1 || { cat /tmp/callback-authentication-build.out >&2; exit 1; }")
    output_path = stock_client.succeed("tail -n 1 /tmp/callback-authentication-build.out").strip()
    stock_client.succeed("test \"$(cat '" + output_path + "')\" = callback-authenticated")
    gateway.succeed("nix-store --verify-path '" + output_path + "'")
  '';
}
