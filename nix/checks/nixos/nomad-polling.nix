# Measures status requests and live log delivery against a real Nomad allocation.
{
  pkgs,
  telchar,
  nomadWorker,
  ...
}:
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
        if mode == "chatty":
            script += "for i in $(seq 1 80); do echo POLL_CHUNK_$i $(date +%s%N) >&2; sleep 0.1; done; "
        else:
            script += "sleep " + ("60" if mode == "cancel" else "8") + "; "
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
            marker = "^POLL_CHUNK_1 " if mode == "chatty" else "^POLL_STARTED$"
            stock_client.wait_until_succeeds("journalctl -u poll-" + mode + " --no-pager -o cat | grep " + shlex.quote(marker), timeout=90)
            stock_client.succeed("systemctl is-active poll-" + mode)
            stock_client.fail("journalctl -u poll-" + mode + " --no-pager -o cat | grep -qx POLL_FINISHED")
        if mode == "cancel":
            job_id = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc " + shlex.quote("select backend_execution_id from shared_builds where derivation_path = '" + drv + "'")).strip()
            allocations = json.loads(nomad_server.succeed("nomad job allocs -namespace telchar -json " + shlex.quote(job_id)))
            assert len(allocations) == 1 and allocations[0]["ClientStatus"] == "running", allocations
            allocation_id = allocations[0]["ID"]
            find_worker = """
    import json, pathlib, sys
    matches = []
    for proc in pathlib.Path('/proc').glob('[0-9]*'):
        try:
            environment = (proc / 'environ').read_bytes().split(b'\\0')
            executable = pathlib.Path((proc / 'exe').readlink()).name
            if ('NOMAD_ALLOC_ID=' + sys.argv[1]).encode() in environment and executable == 'telchar-nomad-worker':
                start = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
                matches.append(dict(pid=int(proc.name), start=start))
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            pass
    assert len(matches) == 1, matches
    pathlib.Path('/tmp/cancel-worker.json').write_text(json.dumps(matches[0]))
    print(json.dumps(matches[0]))
    """
            worker_identity = nomad_client.succeed("${pkgs.python3}/bin/python3 -c " + shlex.quote(find_worker) + " " + shlex.quote(allocation_id)).strip()
            stock_client.succeed("systemctl stop poll-cancel")
            gateway.wait_until_succeeds("sudo -u postgres psql -d telchar-ingress -Atc " + shlex.quote("select state from shared_builds where derivation_path = '" + drv + "'") + " | grep -qx failed", timeout=30)
            cancel_journal = gateway.succeed("journalctl --sync; journalctl -u telchar-daemon --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
            assert any('event="nomad.api.request.completed"' in line and 'operation="stop"' in line and 'result="succeeded"' in line for line in cancel_journal.splitlines()), cancel_journal
            for resource in ["allocation/" + allocation_id, "job/" + job_id]:
                query = "${pkgs.curl}/bin/curl --silent --show-error -o /dev/null -w '%{http_code}' " + shlex.quote("http://192.168.1.1:4646/v1/" + resource + "?namespace=telchar")
                nomad_server.wait_until_succeeds("test $(" + query + ") = 404", timeout=30)
            worker_gone = """
    import json, pathlib
    identity = json.loads(pathlib.Path('/tmp/cancel-worker.json').read_text())
    try:
        start = pathlib.Path('/proc', str(identity['pid']), 'stat').read_text().rsplit(')', 1)[1].split()[19]
    except (FileNotFoundError, ProcessLookupError):
        start = None
    assert start != identity['start'], identity
    """
            nomad_client.wait_until_succeeds("${pkgs.python3}/bin/python3 -c " + shlex.quote(worker_gone), timeout=30)
            print("NOMAD_POLL_ALLOCATION_PURGED worker_terminated " + worker_identity)
            gateway.succeed("test ! -e " + shlex.quote(output))
            print("NOMAD_POLL_CANCELLATION verified real allocation stop after requester disconnect")
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
        assert len(completed_polls) == len(polls) and len(completed_polls) > 0, events
        results.append(dict(mode=mode, log_interarrival_us=spacing_us, finish_to_result_us=completion - finish, status_polls=len(polls), status_http_gets=2 * len(completed_polls), poll_timestamps_us=[int(e["__MONOTONIC_TIMESTAMP"]) for e in polls]))
        print("NOMAD_POLL_BENCHMARK " + json.dumps(results[-1]))
        gateway.succeed("test $(cat " + shlex.quote(output) + ") = " + nonce + "; nix-store --verify-path " + shlex.quote(output))
    print("NOMAD_POLL_RESULTS " + json.dumps(results))
  '';
}
