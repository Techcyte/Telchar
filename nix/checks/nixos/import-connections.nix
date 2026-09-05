# Checks cold SSH imports against a real daemon and records reference-heavy transfer timings.
{ pkgs, system, telchar, telcharModule, ... }:
pkgs.testers.nixosTest {
  name = "telchar-nixos-import-connections";
  nodes = {
    client = { ... }: {
      nix.settings.experimental-features = [ "nix-command" ];
      system.stateVersion = "26.05";
    };
    gateway = { ... }: {
      imports = [ telcharModule ];
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
        environment.TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
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
    for references in [0, 8]:
        for frontend in ["plain", "telchar", "telchar", "plain"]:
            expression = """let nodes = builtins.genList (i: builtins.derivation {
              name = "import-probe"; system = "${system}"; builder = "/bin/sh";
              seed = "SEED"; index = builtins.toString i;
              dependencies = builtins.genList (j: (builtins.elemAt nodes (i - j - 1)).outPath)
                (if i < REFS then i else REFS);
            }) 100; in nodes""".replace("SEED", uuid.uuid4().hex).replace("REFS", str(references))
            # Indirect roots protect the fresh source closure until the VM exits.
            client.succeed("rm -f /tmp/import-root*")
            client.succeed("nix-instantiate --add-root /tmp/import-root --indirect --expr " + shlex.quote(expression) + " >/dev/null")
            paths = client.succeed("readlink -f /tmp/import-root*").splitlines()
            assert len(paths) == 100, paths
            gateway.succeed("; ".join("test ! -e " + shlex.quote(path) for path in paths))
            cursor = gateway.succeed("journalctl -u nix-daemon.service -n 0 --show-cursor --no-pager").strip().split("-- cursor: ")[1]
            endpoint = "'ssh-ng://root@gateway?remote-store=daemon'" if frontend == "plain" else "ssh-ng://telchar@gateway:2222"
            started = time.monotonic()
            client.succeed("NIX_SSHOPTS='-4' nix copy --to " + endpoint + " " + " ".join(paths), timeout=120)
            elapsed = time.monotonic() - started
            journal = gateway.succeed("journalctl --sync; journalctl -u nix-daemon.service --after-cursor=" + shlex.quote(cursor) + " --no-pager -o cat")
            connections = journal.count("accepted connection from pid ")
            results.append(dict(frontend=frontend, references=references, seconds=elapsed, connections=connections))
            print("IMPORT_BENCHMARK " + json.dumps(results[-1]))
            gateway.succeed("nix-store --verify-path " + " ".join(paths))
    print("IMPORT_BENCHMARK_RESULTS " + json.dumps(results))
    for result in results:
        assert result["connections"] == (1 if result["frontend"] == "plain" else 2), result
  '';
}
