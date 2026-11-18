# Stock SSH clients upload locally built input-addressed paths through the real daemon.
{
  pkgs,
  system,
  telchar,
  telcharModule,
  ...
}:
let
  # Retained in both independent stores, so only the fresh test outputs need upload trust.
  builder = pkgs.writeShellScript "upload-verification-builder" ''
    if [ -n "''${dependency-}" ]; then
      read -r value < "$dependency"
      read -r suffix < "$source"
      printf 'consumer:%s:%s\n' "$value" "$suffix" > "$out"
    else
      printf 'locally-built-input\n' > "$out"
    fi
  '';
  fixture = pkgs.writeText "upload-verification.nix" ''
    let
      input = builtins.derivation {
        name = "upload-verification-input";
        system = "${system}";
        builder = builtins.storePath "${builder}";
      };
    in {
      inherit input;
      copyInput = builtins.derivation {
        name = "upload-verification-copy-input";
        system = "${system}";
        builder = builtins.storePath "${builder}";
      };
      consumer = builtins.derivation {
        name = "upload-verification-consumer";
        system = "${system}";
        builder = builtins.storePath "${builder}";
        dependency = input;
        source = builtins.toFile "upload-verification-source" "content-addressed-source\n";
      };
    }
  '';
  emptyRegistry = pkgs.writeText "empty-upload-key-registry.json" ''{"version":1,"keys":[]}'';
  initializeRegistry = pkgs.writeShellScript "initialize-upload-key-registry" ''
    ${pkgs.coreutils}/bin/install -o root -g root -m 0644 ${emptyRegistry} /var/lib/telchar/trusted-keys.json
  '';
in
pkgs.testers.nixosTest {
  name = "telchar-nixos-upload-verification";
  nodes = {
    client = { ... }: {
      nix.settings = {
        experimental-features = [ "nix-command" ];
        substituters = [ ];
      };
      environment.etc."upload-fixture.nix".source = fixture;
      environment.etc."upload-builder".source = builder;
      system.stateVersion = "26.05";
    };
    gateway = { ... }: {
      imports = [ telcharModule ];
      environment.etc."upload-builder".source = builder;
      networking.firewall.enable = false;
      users.users.telchar.hashedPassword = "*";
      services.openssh.enable = true;
      nix.settings = {
        experimental-features = [ "nix-command" ];
        substituters = [ ];
      };
      systemd.services.telchar.serviceConfig.ExecStartPre = pkgs.lib.mkAfter [ "+${initializeRegistry}" ];
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
        settings = {
          audit.enabled = true;
          upload_verification = {
            enabled = true;
            min_signatures = 1;
            exempt_content_addressed = true;
            identity_match = "off";
            resolver = {
              file = "/var/lib/telchar/trusted-keys.json";
              refresh_interval = "1h";
              unknown_key_refresh_limit = "6/min";
            };
          };
          backends.local = {
            name = "local";
            inherit system;
            maximum_concurrent_builds = 1;
          };
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

    start_all()
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_unit("telchar-sshd.service")
    gateway.succeed("test $(stat -c '%U:%G:%a' /var/lib/telchar/trusted-keys.json) = root:root:644")
    assert json.loads(gateway.succeed("cat /var/lib/telchar/trusted-keys.json")) == {"version": 1, "keys": []}
    client.succeed('mkdir -p /root/.ssh; ssh-keygen -q -t ed25519 -N "" -f /root/.ssh/id_ed25519')
    ssh_key = client.succeed("cat /root/.ssh/id_ed25519.pub").strip()
    gateway.succeed("mkdir -p /etc/ssh/authorized_keys.d; printf '%s\\n' " + shlex.quote(ssh_key) + " > /etc/ssh/authorized_keys.d/telchar")
    client.succeed("ssh-keyscan -4 -t ed25519 -p 2222 gateway > /root/.ssh/known_hosts 2>/dev/null")
    endpoint = "ssh-ng://telchar@gateway:2222"
    transport = "HOME=/root NIX_SSHOPTS='-4' "

    def path_info(machine, path):
        return json.loads(machine.succeed("nix path-info --json --json-format 1 --sigs " + shlex.quote(path)))[path]

    def audit_events():
        journal = gateway.succeed("journalctl --sync; journalctl -u telchar.service --no-pager -o cat")
        return [json.loads(line[line.index("{"):]) for line in journal.splitlines() if "WARN audit." in line and "{" in line]

    drvs = {}
    outputs = {}
    for name in ["input", "copyInput", "consumer"]:
        drvs[name] = client.succeed("nix-instantiate /etc/upload-fixture.nix -A " + name).strip()
        outputs[name] = client.succeed("nix-store --query --outputs " + shlex.quote(drvs[name])).strip()
    for name in ["input", "copyInput"]:
        client.succeed("nix build --no-link --builders " + shlex.quote("") + " --max-jobs 1 " + shlex.quote(drvs[name] + "^*"))
        info = path_info(client, outputs[name])
        assert not info.get("ca") and not info["signatures"], info
        client.succeed("test $(cat " + shlex.quote(outputs[name]) + ") = locally-built-input")
    source_path = client.succeed("nix-instantiate --eval --strict --json --expr '(import /etc/upload-fixture.nix).consumer.source'").strip()
    source_path = json.loads(source_path)
    assert path_info(client, source_path).get("ca"), source_path
    for path in list(outputs.values()) + [source_path, drvs["consumer"]]:
        gateway.succeed("test ! -e " + shlex.quote(path))
    client.succeed("test ! -e " + shlex.quote(outputs["consumer"]))

    copy = transport + "nix copy --to " + endpoint + " " + shlex.quote(outputs["copyInput"])
    build = transport + "nix build --no-link --print-out-paths --max-jobs 0 --builders " + shlex.quote(endpoint + " ${system}") + " " + shlex.quote(drvs["consumer"] + "^*")
    for name, command in [("copy", copy), ("build", build)]:
        client.fail(command + " >/tmp/unsigned-" + name + ".out 2>/tmp/unsigned-" + name + ".err")
        stderr = client.succeed("cat /tmp/unsigned-" + name + ".err")
        assert "rejected" in stderr and "InsufficientSignatures" in stderr, stderr

    # A normal stock-client failure is terminal here: it does not rebuild the rejected input.
    for path in outputs.values():
        gateway.succeed("test ! -e " + shlex.quote(path))
    client.succeed("test ! -e " + shlex.quote(outputs["consumer"]))
    rejected = audit_events()
    for name in ["input", "copyInput"]:
        assert any(event["event"] == "upload.rejected" and event["store_path"] == outputs[name] and event["verification"]["reason"] == "insufficient_signatures" and event["verification"]["validSignatures"] == 0 for event in rejected), rejected
    assert not any(event["event"] == "build.requested" for event in rejected), rejected

    # The only secret stays on the client, mode 0600, and is never returned to the driver.
    client.succeed("umask 077; nix key generate-secret --key-name upload-client-1 > /root/upload-signing-key; nix key convert-secret-to-public < /root/upload-signing-key > /root/upload-signing-key.pub")
    client.succeed("test $(stat -c %a /root/upload-signing-key) = 600")
    public_key = client.succeed("cat /root/upload-signing-key.pub").strip()
    registry = json.dumps({"version": 1, "keys": [{"publicKey": public_key, "metadata": {"identity": "upload-client", "kind": "host"}}]})
    # Publish atomically after startup; the next unknown signer can refresh the live registry.
    gateway.succeed("umask 022; printf '%s\\n' " + shlex.quote(registry) + " > /var/lib/telchar/trusted-keys.json.new; chown root:root /var/lib/telchar/trusted-keys.json.new; chmod 644 /var/lib/telchar/trusted-keys.json.new; mv /var/lib/telchar/trusted-keys.json.new /var/lib/telchar/trusted-keys.json")
    for name in ["input", "copyInput"]:
        client.succeed("nix store sign -k /root/upload-signing-key " + shlex.quote(outputs[name]))
        info = path_info(client, outputs[name])
        assert not info.get("ca") and len(info["signatures"]) == 1 and info["signatures"][0].startswith("upload-client-1:"), info
    client.succeed("rm /root/upload-signing-key")

    client.succeed(copy)
    client.succeed(transport + "nix copy --to " + endpoint + " " + shlex.quote(drvs["consumer"]))
    # The separate copy probe did not preseed the consumer's dependency.
    gateway.succeed("test ! -e " + shlex.quote(outputs["input"]))
    actual_output = client.succeed(build).strip()
    assert actual_output == outputs["consumer"], (actual_output, outputs)
    gateway.succeed("test $(cat " + shlex.quote(actual_output) + ") = consumer:locally-built-input:content-addressed-source")
    gateway.succeed("nix-store --verify-path " + " ".join(shlex.quote(path) for path in outputs.values()))
    for name in ["input", "copyInput"]:
        source = path_info(client, outputs[name])
        destination = path_info(gateway, outputs[name])
        for field in ["narHash", "narSize", "references"]:
            assert source[field] == destination[field], (name, field, source, destination)

    gateway.wait_until_succeeds("journalctl --sync; journalctl -u telchar.service --no-pager -o cat | grep -F 'build.completed'")
    events = audit_events()
    for name in ["input", "copyInput"]:
        accepted = [event for event in events if event["event"] == "upload.accepted" and event["store_path"] == outputs[name]]
        assert accepted, events
        for event in accepted:
            verification = event["verification"]
            assert verification["reason"] == "signatures_verified" and verification["validSignatures"] == 1, event
            signer = verification["signatures"][0]
            assert signer["keyName"] == "upload-client-1" and signer["status"] == "valid" and signer["source"] == "file", event
            assert signer["metadata"]["identity"] == "upload-client" and signer["metadata"]["kind"] == "host", event
            assert event["session_id"] and event["audit_subject"] and event["credential_id"], event
    # The .drv and fresh text input use the CA exemption and real Nix registration, not a test import.
    for path in [drvs["consumer"], source_path]:
        assert any(event["event"] == "upload.accepted" and event["store_path"] == path and event["verification"]["reason"] == "content_addressed_exemption" for event in events), {"missing_ca_upload": path, "uploaded_paths": [event["store_path"] for event in events if event["event"] == "upload.accepted"]}
        gateway.succeed("nix-store --verify-path " + shlex.quote(path))
    assert any(event["event"] == "build.completed" and outputs["consumer"] in event["output_paths"] for event in events), events
  '';
}
