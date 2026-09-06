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
    for mode in ["quiet", "chatty", "cancel"]:
        if mode == "cancel":
            gateway.succeed("sed -i 's/detach-and-finish/cancel-running/' /var/lib/telchar-import/telchar.toml; systemctl restart telchar-daemon")
        nonce = uuid.uuid4().hex
        script = "set -eu; export PATH=$tools/bin; echo POLL_STARTED >&2; "
        if mode in ["chatty", "cancel"]:
            script += "for i in $(seq 1 80); do echo POLL_CHUNK_$i $(date +%s%N) >&2; sleep 0.1; done; "
        else:
            script += "sleep 8; "
        script += "echo POLL_FINISHED >&2; printf " + nonce + " > $out"
        expression = 'derivation { name = "poll-' + nonce + '"; system = "${pkgs.stdenv.hostPlatform.system}"; builder = builtins.storePath "${pkgs.runtimeShell}"; tools = builtins.storePath "${pkgs.coreutils}"; args = [ "-c" ' + json.dumps(script) + ' ]; }'
        drv = stock_client.succeed("nix-instantiate --expr " + shlex.quote(expression)).strip()
        output = stock_client.succeed("nix-store -q --outputs " + shlex.quote(drv)).strip()
        gateway.succeed("test ! -e " + shlex.quote(output))
        exported = stock_client.succeed("nix-store --export " + shlex.quote(drv) + " | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf %s " + shlex.quote(exported) + " | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        cursor = gateway.succeed("journalctl -u telchar-daemon -n 0 --show-cursor --no-pager").strip().split("-- cursor: ")[1]
        build = "PATH=/run/current-system/sw/bin HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes' nix --extra-experimental-features nix-command build -L --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' " + shlex.quote(drv + "^*")
        stock_client.succeed("systemd-run --unit=poll-" + mode + " ${pkgs.bash}/bin/bash -c " + shlex.quote(build))
        if mode in ["chatty", "cancel"]:
            stock_client.wait_until_succeeds("journalctl -u poll-" + mode + " --no-pager -o cat | grep '^POLL_CHUNK_1 '", timeout=90)
            stock_client.succeed("systemctl is-active poll-" + mode)
            stock_client.fail("journalctl -u poll-" + mode + " --no-pager -o cat | grep -qx POLL_FINISHED")
        if mode == "cancel":
            stock_client.succeed("systemctl stop poll-cancel")
            gateway.wait_until_succeeds("sudo -u postgres psql -d telchar-ingress -Atc " + shlex.quote("select state from shared_builds where derivation_path = '" + drv + "'") + " | grep -qx failed", timeout=30)
            cancel_journal = gateway.succeed("journalctl --sync; journalctl -u telchar-daemon --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
            assert 'Nomad job execution cancelled' in cancel_journal, cancel_journal
            gateway.succeed("test ! -e " + shlex.quote(output))
            print("NOMAD_POLL_CANCELLATION verified real allocation cancellation after live log")
            continue
        stock_client.wait_until_fails("systemctl is-active poll-" + mode, timeout=120)
        stock_client.succeed("test $(systemctl show -p ExecMainStatus --value poll-" + mode + ") = 0")
        build_events = [json.loads(line) for line in stock_client.succeed("journalctl --sync; journalctl -u poll-" + mode + " --no-pager -o json").splitlines() if line.startswith("{")]
        chunks = [e for e in build_events if e.get("MESSAGE", "").startswith("POLL_CHUNK_")]
        assert len(chunks) == (80 if mode == "chatty" else 0), build_events
        assert any(e.get("MESSAGE") == "POLL_FINISHED" for e in build_events), build_events
        received = [int(e["__MONOTONIC_TIMESTAMP"]) for e in chunks]
        spacing_us = [right - left for left, right in zip(received, received[1:])]
        finish = next(int(e["__MONOTONIC_TIMESTAMP"]) for e in build_events if e.get("MESSAGE") == "POLL_FINISHED")
        completion = next(int(e["__MONOTONIC_TIMESTAMP"]) for e in build_events if e.get("MESSAGE") == output)
        journal = gateway.succeed("journalctl --sync; journalctl -u telchar-daemon --after-cursor=" + shlex.quote(cursor) + " --no-pager -o json")
        events = [json.loads(line) for line in journal.splitlines() if line.startswith("{")]
        polls = [e for e in events if 'event="nomad.api.request.started"' in e.get("MESSAGE", "") and 'operation="status"' in e["MESSAGE"]]
        completed_polls = [e for e in events if 'event="nomad.api.request.completed"' in e.get("MESSAGE", "") and 'operation="status"' in e["MESSAGE"]]
        assert len(completed_polls) == len(polls), events
        results.append(dict(mode=mode, log_interarrival_us=spacing_us, finish_to_result_us=completion - finish, status_polls=len(polls), status_http_gets=2 * len(completed_polls), poll_timestamps_us=[int(e["__MONOTONIC_TIMESTAMP"]) for e in polls]))
        print("NOMAD_POLL_BENCHMARK " + json.dumps(results[-1]))
        gateway.succeed("test $(cat " + shlex.quote(output) + ") = " + nonce + "; nix-store --verify-path " + shlex.quote(output))
    print("NOMAD_POLL_RESULTS " + json.dumps(results))
  '';
}
