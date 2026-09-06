# Checks cold SSH downloads against a real daemon and records connection counts.
{
  pkgs,
  system,
  telchar,
  telcharModule,
  ...
}:
let
  exportTests = telchar.overrideAttrs (previous: {
    postInstall = (previous.postInstall or "") + ''
      for executable in target/release/deps/telchar-*; do
        if test -f "$executable" && test -x "$executable"; then
          install -Dm755 "$executable" "$out/bin/telchar-export-tests"
        fi
      done
    '';
  });
in
pkgs.testers.nixosTest {
  name = "telchar-nixos-export-connections";
  nodes = {
    client = { ... }: {
      nix.settings.experimental-features = [ "nix-command" ];
      environment.systemPackages = [ pkgs.python3 ];
      system.stateVersion = "26.05";
    };
    gateway = { ... }: {
      imports = [ telcharModule ];
      environment.systemPackages = [
        pkgs.python3
        exportTests
      ];
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
    for workload in ["derivations", "references", "small-files", "large-file"]:
        for repetition in range(3):
            for frontend in (["plain", "telchar"] if repetition % 2 == 0 else ["telchar", "plain"]):
                nonce = uuid.uuid4().hex
                references = 8 if workload == "references" else 0
                gateway.succeed("rm -f /tmp/export-root*")
                if workload in ["derivations", "references"]:
                    expression = """let nodes = builtins.genList (i: builtins.derivation {
                      name = "export-probe"; system = "${system}"; builder = "/bin/sh";
                      seed = "SEED"; index = builtins.toString i;
                      dependencies = builtins.genList (j: (builtins.elemAt nodes (i - j - 1)).outPath)
                        (if i < REFS then i else REFS);
                    }) 100; in nodes""".replace("SEED", nonce).replace("REFS", str(references))
                    gateway.succeed("nix-instantiate --add-root /tmp/export-root --indirect --expr " + shlex.quote(expression) + " >/dev/null")
                    paths = gateway.succeed("readlink -f /tmp/export-root*").splitlines()
                    assert len(paths) == 100, paths
                else:
                    source = "/tmp/export-input-" + nonce
                    files, size = (1000, 4096) if workload == "small-files" else (1, 16 * 1024 * 1024)
                    script = "import pathlib; root = pathlib.Path(" + repr(source) + "); root.mkdir(); "
                    script += "[(root / str(i)).write_bytes((" + repr(nonce.encode()) + " * " + str(size // len(nonce)) + ")) for i in range(" + str(files) + ")]"
                    gateway.succeed("python3 -c " + shlex.quote(script))
                    path = gateway.succeed("nix-store --add " + shlex.quote(source)).strip()
                    gateway.succeed("nix-store --realise --add-root /tmp/export-root --indirect " + shlex.quote(path) + " >/dev/null")
                    paths = [path]
                source_info = json.loads(gateway.succeed("nix path-info --json --json-format 1 " + " ".join(paths)))
                gateway.succeed("nix-store --verify-path " + " ".join(paths))
                client.succeed(" && ".join("test ! -e " + shlex.quote(path) for path in paths))
                cursor = gateway.succeed("journalctl -u nix-daemon.service -n 0 --show-cursor --no-pager").strip().split("-- cursor: ")[1]
                endpoint = "'ssh-ng://root@gateway?remote-store=daemon'" if frontend == "plain" else "ssh-ng://telchar@gateway:2222"
                started = time.monotonic()
                client.succeed("NIX_SSHOPTS='-4' nix copy " + ("--derivation " if workload in ["derivations", "references"] else "") + "--from " + endpoint + " " + " ".join(paths), timeout=120)
                elapsed = time.monotonic() - started
                journal = gateway.succeed("journalctl --sync; journalctl -u nix-daemon.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
                connections = journal.count("accepted connection from pid ")
                results.append(dict(workload=workload, repetition=repetition, frontend=frontend, references=references, paths=len(paths), nar_bytes=sum(info["narSize"] for info in source_info.values()), seconds=elapsed, connections=connections))
                print("EXPORT_BENCHMARK " + json.dumps(results[-1]))
                client.succeed("nix-store --verify-path " + " ".join(paths))
                destination_info = json.loads(client.succeed("nix path-info --json --json-format 1 " + " ".join(paths)))
                assert source_info.keys() == destination_info.keys()
                for path in paths:
                    for field in ["narHash", "narSize", "references"]:
                        assert source_info[path][field] == destination_info[path][field], (path, field)
    print("EXPORT_BENCHMARK_RESULTS " + json.dumps(results))
    for result in results:
        # Non-derivation copy also performs two independent validity queries.
        expected = 3 if result["frontend"] == "telchar" and result["paths"] == 1 else 1
        assert result["connections"] == expected, result
    gateway.succeed("printf '%s' " + shlex.quote(uuid.uuid4().hex) + " > /tmp/export-verification.drv")
    path = gateway.succeed("nix-store --add /tmp/export-verification.drv").strip()
    gateway.succeed("TELCHAR_EXPORT_TEST_STORE=unix:///nix/var/nix/daemon-socket/socket TELCHAR_EXPORT_TEST_PATH=" + path + " telchar-export-tests store::export::tests::verified_export_failure_discards_connection --ignored --nocapture")
    gateway.succeed("nix-store --verify-path " + path)
  '';
}
