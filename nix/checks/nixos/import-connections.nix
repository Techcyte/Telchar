# Checks cold SSH imports against a real daemon and records reference-heavy transfer timings.
{
  pkgs,
  system,
  telchar,
  telcharModule,
  traceQueries ? false,
  ...
}:
pkgs.testers.nixosTest {
  name = "telchar-nixos-import-connections";
  nodes = {
    client = { ... }: {
      nix.settings.experimental-features = [ "nix-command" ];
      environment.systemPackages = [ pkgs.python3 ];
      environment.etc."query-valid-paths.py".source = ../../tests/query-valid-paths.py;
      system.stateVersion = "26.05";
    };
    gateway = { ... }: {
      imports = [ telcharModule ];
      environment.systemPackages = [ pkgs.python3 ] ++ pkgs.lib.optionals traceQueries [ pkgs.strace ];
      environment.etc."query-valid-paths.py".source = ../../tests/query-valid-paths.py;
      systemd.services.telchar-sshd.serviceConfig.ExecStart = pkgs.lib.mkIf traceQueries (
        pkgs.lib.mkForce "${pkgs.openssh}/bin/sshd -D -e -f /etc/telchar/sshd_config -o SetEnv=RUST_LOG=info,telchar::service=trace,telchar::runtime=trace"
      );
      networking.firewall.enable = false;
      users.users.telchar.hashedPassword = "*";
      services.openssh.enable = true;
      services.openssh.settings.PermitRootLogin = "prohibit-password";
      nix.settings.experimental-features = [ "nix-command" ];
      nix.settings.substituters = pkgs.lib.mkForce [ "file:///var/lib/query-cache" ];
      services.telchar = {
        enable = true;
        package = telchar;
        frontendUid = 995;
        database.manage = true;
        gatewayStore.manageTrustedUser = true;
        gatewayStore.manageGcRootDirectory = true;
        ingress.openssh = {
          enable = true;
          port = 2222;
          hostKeyFile = "/etc/ssh/ssh_host_ed25519_key";
          authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
        };
        settings.backends.local = {
          name = "local";
          inherit system;
          maximum_concurrent_builds = 1;
        };
        environment = {
          TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
        } // pkgs.lib.optionalAttrs traceQueries {
          RUST_LOG = "info,telchar::store=trace,telchar::service=trace,telchar::runtime=trace";
        };
      };
      virtualisation.memorySize = 2048;
      system.stateVersion = "26.05";
    };
  };
  testScript = ''
    import json
    import shlex
    import time
    import uuid

    start_all()
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_unit("telchar-sshd.service")
    client.succeed("mkdir -p /root/.ssh; ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/id_ed25519")
    key = client.succeed("cat /root/.ssh/id_ed25519.pub").strip()
    gateway.succeed("mkdir -p /root/.ssh /etc/ssh/authorized_keys.d; printf '%s\\n' " + shlex.quote(key) + " > /root/.ssh/authorized_keys; cp /root/.ssh/authorized_keys /etc/ssh/authorized_keys.d/telchar")
    client.succeed("ssh-keyscan -4 -t ed25519 gateway > /root/.ssh/known_hosts 2>/dev/null; ssh-keyscan -4 -t ed25519 -p 2222 gateway >> /root/.ssh/known_hosts 2>/dev/null")
    gateway.succeed("nix --store daemon store info >/dev/null; journalctl --sync")
    results = []
    tracing_queries = ${if traceQueries then "True" else "False"}
    for workload in (["large-file", "derivations"] if tracing_queries else ["derivations", "references", "small-files", "large-file"]):
        for repetition, frontend in enumerate(["telchar"] * 3 if tracing_queries else ["plain", "telchar"] * 3):
            nonce = uuid.uuid4().hex
            references = 8 if workload == "references" else 0
            # Indirect roots protect the fresh source closure until the VM exits.
            client.succeed("rm -f /tmp/import-root*")
            if workload in ["derivations", "references"]:
                expression = """let nodes = builtins.genList (i: builtins.derivation {
                  name = "import-probe"; system = "${system}"; builder = "/bin/sh";
                  seed = "SEED"; index = builtins.toString i;
                  dependencies = builtins.genList (j: (builtins.elemAt nodes (i - j - 1)).outPath)
                    (if i < REFS then i else REFS);
                }) 100; in nodes""".replace("SEED", nonce).replace("REFS", str(references))
                client.succeed("nix-instantiate --add-root /tmp/import-root --indirect --expr " + shlex.quote(expression) + " >/dev/null")
                paths = client.succeed("readlink -f /tmp/import-root*").splitlines()
                assert len(paths) == 100, paths
            else:
                source = "/tmp/input-" + nonce
                files, size = (1000, 4096) if workload == "small-files" else (1, 16 * 1024 * 1024)
                script = "import pathlib; root = pathlib.Path(" + repr(source) + "); root.mkdir(); "
                script += "[(root / str(i)).write_bytes((" + repr(nonce.encode()) + " * " + str(size // len(nonce)) + ")) for i in range(" + str(files) + ")]"
                client.succeed("python3 -c " + shlex.quote(script))
                path = client.succeed("nix-store --add " + shlex.quote(source)).strip()
                client.succeed("nix-store --realise --add-root /tmp/import-root --indirect " + shlex.quote(path) + " >/dev/null")
                paths = [path]
            source_info = json.loads(client.succeed("nix path-info --json --json-format 1 " + " ".join(paths)))
            gateway.succeed(" && ".join("test ! -e " + shlex.quote(path) for path in paths))
            cursor = gateway.succeed("journalctl -u nix-daemon.service -n 0 --show-cursor --no-pager").strip().split("-- cursor: ")[1]
            endpoint = "'ssh-ng://root@gateway?remote-store=daemon'" if frontend == "plain" else "ssh-ng://telchar@gateway:2222"
            if tracing_queries:
                gateway.succeed("pid=$(systemctl show -p MainPID --value telchar); strace -f -e trace=execve -o /tmp/gateway-exec -p $pid 2>/tmp/strace-status & echo $! >/tmp/strace-pid")
                gateway.wait_until_succeeds("grep -q 'attached' /tmp/strace-status")
            started = time.monotonic()
            client.succeed("NIX_SSHOPTS='-4' nix copy --to " + endpoint + " " + " ".join(paths) + (" 2>/tmp/frontend-trace" if tracing_queries else ""), timeout=120)
            elapsed = time.monotonic() - started
            if tracing_queries:
                gateway.succeed("kill -INT $(cat /tmp/strace-pid)")
                gateway.wait_until_succeeds("! kill -0 $(cat /tmp/strace-pid) 2>/dev/null")
                executions = gateway.succeed("cat /tmp/gateway-exec")
                assert "execve(" not in executions, executions
                print("GATEWAY_EXEC_TRACE " + workload + "\n" + executions)
            journal = gateway.succeed("journalctl --sync; journalctl -u nix-daemon.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
            connections = journal.count("accepted connection from pid ")
            results.append(dict(workload=workload, repetition=repetition if tracing_queries else repetition // 2, frontend=frontend, references=references, paths=len(paths), nar_bytes=sum(info["narSize"] for info in source_info.values()), seconds=elapsed, connections=connections))
            print("IMPORT_BENCHMARK " + json.dumps(results[-1]))
            if tracing_queries:
                traces = gateway.succeed("journalctl --sync; journalctl -u telchar.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o short-monotonic")
                print("QUERY_TIMELINE " + workload + "\n" + traces)
                frontend_trace = client.succeed("cat /tmp/frontend-trace")
                print("FRONTEND_TIMELINE " + workload + "\n" + frontend_trace)
                assert 'event="ipc.frontend.envelope_sent"' in frontend_trace, frontend_trace
                assert 'event="ipc.relay.first_bytes"' in frontend_trace, frontend_trace
                session = next(line.split("session_id=")[1].strip() for line in frontend_trace.splitlines() if 'event="ipc.frontend.envelope_sent"' in line)
                assert 'event="ipc.daemon.session_received" session_id=' + session in traces, traces
                assert 'event="store.daemon.query_valid_paths"' in traces, traces
                assert 'event="worker.query_valid_paths.flushed"' in traces, traces
            gateway.succeed("nix-store --verify-path " + " ".join(paths))
            destination_info = json.loads(gateway.succeed("nix path-info --json --json-format 1 " + " ".join(paths)))
            assert source_info.keys() == destination_info.keys()
            for path in paths:
                for field in ["narHash", "narSize", "references"]:
                    assert source_info[path][field] == destination_info[path][field], (path, field)
    # Only the cache retains these fresh paths when validity requests begin.
    for transport in ["daemon", "ssh"]:
        nonce = uuid.uuid4().hex
        cache_source = "/tmp/cache-input-" + nonce
        gateway.succeed("printf '%s' " + shlex.quote(nonce) + " > " + cache_source)
        cached_path = gateway.succeed("nix-store --add " + cache_source).strip()
        gateway.succeed("nix copy --to file:///var/lib/query-cache " + cached_path)
        gateway.succeed("nix-store --delete " + cached_path)
        gateway.succeed("test ! -e " + cached_path)
        if transport == "daemon":
            report = gateway.succeed("runuser -u telchar -- python3 /etc/query-valid-paths.py /nix/var/nix/daemon-socket/socket " + cached_path)
        else:
            report = client.succeed("python3 /etc/query-valid-paths.py ssh " + cached_path)
        print("CACHE_VALIDITY " + report)
        gateway.succeed("test $(cat " + cached_path + ") = " + shlex.quote(nonce))
        gateway.succeed("nix-store --verify-path " + cached_path)
    print("IMPORT_BENCHMARK_RESULTS " + json.dumps(results))
    for result in results:
        assert result["connections"] == (1 if result["frontend"] == "plain" else 2), result
  '';
}
