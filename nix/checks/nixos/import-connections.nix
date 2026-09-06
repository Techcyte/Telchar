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
      system.stateVersion = "26.05";
    };
    gateway = { ... }: {
      imports = [ telcharModule ];
      environment.systemPackages = pkgs.lib.optionals traceQueries [ pkgs.strace ];
      networking.firewall.enable = false;
      users.users.telchar.hashedPassword = "*";
      services.openssh.enable = true;
      services.openssh.settings.PermitRootLogin = "prohibit-password";
      nix.settings.experimental-features = [ "nix-command" ];
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
        for repetition, frontend in enumerate(["telchar"] if tracing_queries else ["plain", "telchar"] * 3):
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
            started = time.monotonic()
            client.succeed("NIX_SSHOPTS='-4' nix copy --to " + endpoint + " " + " ".join(paths), timeout=120)
            elapsed = time.monotonic() - started
            journal = gateway.succeed("journalctl --sync; journalctl -u nix-daemon.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
            connections = journal.count("accepted connection from pid ")
            results.append(dict(workload=workload, repetition=repetition // 2, frontend=frontend, references=references, paths=len(paths), nar_bytes=sum(info["narSize"] for info in source_info.values()), seconds=elapsed, connections=connections))
            print("IMPORT_BENCHMARK " + json.dumps(results[-1]))
            if tracing_queries:
                traces = gateway.succeed("journalctl --sync; journalctl -u telchar.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o short-monotonic")
                print("QUERY_TIMELINE " + workload + "\n" + traces)
                assert 'event="store.query.wait"' in traces, traces
                assert 'event="worker.query_valid_paths.flushed"' in traces, traces
            gateway.succeed("nix-store --verify-path " + " ".join(paths))
            destination_info = json.loads(gateway.succeed("nix path-info --json --json-format 1 " + " ".join(paths)))
            assert source_info.keys() == destination_info.keys()
            for path in paths:
                for field in ["narHash", "narSize", "references"]:
                    assert source_info[path][field] == destination_info[path][field], (path, field)
    if tracing_queries:
        for mode in ["path-info", "check-validity"]:
            for repetition in range(3):
                nonce = uuid.uuid4().hex
                path = "/nix/store/" + "a" * 32 + "-query-" + nonce
                gateway.succeed("test ! -e " + shlex.quote(path))
                command = ["nix", "path-info", "--json", "--json-format", "1"] if mode == "path-info" else ["nix-store", "--check-validity"]
                command += ["--store", "unix:///nix/var/nix/daemon-socket/socket", path]
                shell = "runuser -u telchar -- " + shlex.join(command) + " > /tmp/query-stdout 2>/tmp/query-stderr; printf '%s' $?"
                started = time.monotonic()
                status = gateway.succeed(shell).strip()
                print("QUERY_COMPARISON " + json.dumps(dict(mode=mode, repetition=repetition, seconds=time.monotonic()-started, status=status)))
                if mode == "check-validity":
                    assert status == "1", status
                    error = gateway.succeed("cat /tmp/query-stderr")
                    assert "not valid" in error, error
                else:
                    assert status == "0", (status, gateway.succeed("cat /tmp/query-stderr"))
        gateway.succeed("strace -f -ttt -T -o /tmp/query-syscalls runuser -u telchar -- nix path-info --json --json-format 1 --store unix:///nix/var/nix/daemon-socket/socket /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-query-" + uuid.uuid4().hex + " >/tmp/query-stdout 2>/tmp/query-stderr")
        gateway.copy_from_vm("/tmp/query-syscalls")
    print("IMPORT_BENCHMARK_RESULTS " + json.dumps(results))
    for result in results:
        assert result["connections"] == (1 if result["frontend"] == "plain" else 2), result
  '';
}
