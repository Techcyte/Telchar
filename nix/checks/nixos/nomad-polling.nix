# Measures status requests and live log delivery against a real Nomad allocation.
{ pkgs, telchar, nomadWorker, ... }:
let
  harness = import ../../../tests/nixos/lib.nix { inherit pkgs telchar; };
in
harness.mkNomadGatewayTest {
  name = "telchar-nixos-nomad-polling";
  worker = nomadWorker;
  testScript = ''
    import json
    import shlex
    import uuid

    gateway.succeed("sed -i 's/poll_interval_seconds = 1/poll_interval_seconds = 2/' /var/lib/telchar-import/telchar.toml")
    gateway.succeed("mkdir -p /run/systemd/system/telchar-daemon.service.d; printf '%s\\n' '[Service]' 'Environment=RUST_LOG=info,telchar::nomad::backend=trace' > /run/systemd/system/telchar-daemon.service.d/trace.conf; systemctl daemon-reload; systemctl restart telchar-daemon")
    stock_client.succeed("ssh-keyscan -t ed25519 gateway > /root/.ssh/known_hosts 2>/dev/null")
    results = []
    for mode in ["quiet", "chatty"]:
        nonce = uuid.uuid4().hex
        script = "export PATH=${pkgs.coreutils}/bin; echo POLL_STARTED >&2; "
        if mode == "chatty":
            script += "for i in $(seq 1 80); do echo POLL_CHUNK_$i >&2; sleep 0.1; done; "
        else:
            script += "sleep 8; "
        script += "echo POLL_FINISHED >&2; printf " + nonce + " > $out"
        expression = 'derivation { name = "poll-' + nonce + '"; system = "${pkgs.stdenv.hostPlatform.system}"; builder = builtins.storePath "${pkgs.runtimeShell}"; args = [ "-c" ' + json.dumps(script) + ' ]; }'
        drv = stock_client.succeed("nix-instantiate --expr " + shlex.quote(expression)).strip()
        output = stock_client.succeed("nix-store -q --outputs " + shlex.quote(drv)).strip()
        gateway.succeed("test ! -e " + shlex.quote(output))
        exported = stock_client.succeed("nix-store --export " + shlex.quote(drv) + " | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf %s " + shlex.quote(exported) + " | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        cursor = gateway.succeed("journalctl -u telchar-daemon -n 0 --show-cursor --no-pager").strip().split("-- cursor: ")[1]
        build = "PATH=/run/current-system/sw/bin HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes' nix --extra-experimental-features nix-command build -L --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' " + shlex.quote(drv + "^*")
        stock_client.succeed("systemd-run --unit=poll-build --wait --collect --pipe " + "${pkgs.bash}/bin/bash -c " + shlex.quote(build), timeout=120)
        journal = gateway.succeed("journalctl --sync; journalctl -u telchar-daemon --after-cursor=" + shlex.quote(cursor) + " --no-pager -o json")
        events = [json.loads(line) for line in journal.splitlines() if line.startswith("{")]
        polls = [e for e in events if 'event="nomad.api.request.started"' in e.get("MESSAGE", "") and 'operation="status"' in e["MESSAGE"]]
        results.append(dict(mode=mode, status_polls=len(polls), status_http_gets=2 * len(polls), poll_timestamps_us=[int(e["__REALTIME_TIMESTAMP"]) for e in polls]))
        print("NOMAD_POLL_BENCHMARK " + json.dumps(results[-1]))
        gateway.succeed("test $(cat " + shlex.quote(output) + ") = " + nonce + "; nix-store --verify-path " + shlex.quote(output))
    print("NOMAD_POLL_RESULTS " + json.dumps(results))
  '';
}
