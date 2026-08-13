# Defines NixOS VM integration checks for module, ingress, backend, recovery, and artifact behavior.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-module = pkgs.testers.nixosTest {
    name = "telchar-nixos-module";
    nodes.gateway =
      { pkgs, ... }:
      {
        imports = [ telcharModule ];
        networking.firewall.enable = false;
        services.telchar = {
          enable = true;
          package = telchar;
          frontendUid = 995;
          settings = {
            running_disconnect_policy = "detach-and-finish";
            backends.local = {
              name = "local";
              system = system;
              maximum_concurrent_builds = 1;
            };
          };
          environment = {
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };
        environment.etc."ssh/authorized_keys.d/telchar".text =
          "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGQ5k8KfV+TWbrZG7MBXn9cKbIYB1vLLtvbCeK6ucvE3 telchar-module-test\n";
        services.telchar.openssh.authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
        system.stateVersion = "26.05";
      };
    testScript = ''
      start_all()
      gateway.wait_for_unit("postgresql.service")
      gateway.wait_for_unit("telchar.service")
      gateway.succeed("systemctl is-active sshd.service")
      gateway.succeed("systemctl is-active telchar.service || { systemctl status telchar.service --no-pager >&2; journalctl -u telchar.service --no-pager >&2; exit 1; }")
      gateway.succeed("test -S /run/telchar/daemon.sock")
      gateway.succeed("test $(stat -c %a /run/telchar) = 700")
      gateway.succeed("sudo -u postgres psql -Atc \"select 1 from pg_database where datname = 'telchar'\" | grep -qx 1")
      gateway.succeed("systemctl show telchar.service -p User --value | grep -qx telchar")
      gateway.succeed("grep -q 'ForceCommand /nix/store/' /etc/ssh/sshd_config")
    '';
  };
  nixos-test-library =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkTest {
      name = "telchar-nixos-test-library";
      testScript = ''
        start_all()
      '';
    };
  nixos-smoke =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkTest {
      name = "telchar-nixos-smoke";
      includeCollector = true;
      testScript = ''
        start_all()
        otlp_collector.wait_for_open_port(4317)
        gateway.succeed("systemctl restart telchar.service")
        gateway.wait_for_unit("telchar.service")
        stock_client.succeed("ping -c 1 gateway")
        otlp_collector.wait_until_succeeds("test -s /var/lib/telchar-otlp/records.json")
      '';
    };
  nixos-gate-2 =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkTest {
      name = "telchar-nixos-gate-2";
      restrictedIngress = true;
      includeCollector = true;
      testScript = ''
        start_all()
        otlp_collector.wait_for_open_port(4317)
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_for_unit("sshd.service")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ssh_options = "-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar-ingress@gateway"
        stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout 30 nix --extra-experimental-features nix-command --store ssh-ng://telchar-ingress@gateway store info > /tmp/nix-store-info 2>&1")
        stock_client.succeed("grep -q 'Version: telchar' /tmp/nix-store-info || { cat /tmp/nix-store-info >&2; exit 1; }")
        stock_client.succeed("timeout 10 ssh " + ssh_options + " arbitrary-command >/dev/null 2>&1 || true")
        gateway.succeed("grep -q '^original_command=arbitrary-command$' /run/telchar/forced-command-evidence")
        stock_client.succeed("TELCHAR_AUTHENTICATED_KEY=spoofed timeout 10 ssh -o SendEnv=TELCHAR_AUTHENTICATED_KEY " + ssh_options + " ignored >/dev/null 2>&1 || true")
        gateway.succeed("grep -q '^client_supplied_key=$' /run/telchar/forced-command-evidence && ! grep -q '^authenticated_key=spoofed$' /run/telchar/forced-command-evidence")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -tt " + ssh_options + " true >/tmp/pty.out 2>&1; echo $?) -ne 0")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -R 127.0.0.1:22346:127.0.0.1:22 -N " + ssh_options + " >/tmp/remote-forward.out 2>&1; echo $?) -ne 0")
        stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -L 127.0.0.1:22345:127.0.0.1:22 -N " + ssh_options + " >/tmp/local-forward.out 2>&1; echo $?) -ne 0")
      '';
    };
  nixos-nomad-fixture =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkNomadFixtureTest {
      name = "telchar-nixos-nomad-fixture";
      testScript = ''
        nomad_server.succeed("cat > /tmp/telchar-smoke.nomad.hcl <<'EOF'\njob \"telchar-fixture-smoke\" {\n  datacenters = [\"dc1\"]\n  type = \"batch\"\n  group \"smoke\" {\n    task \"write\" {\n      driver = \"raw_exec\"\n      config {\n        command = \"${pkgs.bash}/bin/bash\"\n        args = [\"-c\", \"printf telchar-nomad-fixture > /tmp/telchar-nomad-fixture-output\"]\n      }\n    }\n  }\n}\nEOF\nnomad job run -detach /tmp/telchar-smoke.nomad.hcl")
        nomad_server.wait_until_succeeds("nomad job allocs -json telchar-fixture-smoke | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].ClientStatus == \"complete\"'", timeout=60)
        allocation = nomad_server.succeed("nomad job allocs -json telchar-fixture-smoke | ${pkgs.jq}/bin/jq -r '.[0].ID'").strip()
        nomad_server.succeed("nomad alloc status -json " + allocation + " | ${pkgs.jq}/bin/jq -e '.ClientStatus == \"complete\"'")
        nomad_client.succeed("test \"$(cat /tmp/telchar-nomad-fixture-output)\" = telchar-nomad-fixture")
        nomad_server.succeed("systemctl restart nomad.service")
        nomad_server.wait_until_succeeds("nomad operator raft list-peers | grep -q true", timeout=60)
        nomad_server.wait_until_succeeds("nomad job inspect -json telchar-fixture-smoke | ${pkgs.jq}/bin/jq -e '.ID == \"telchar-fixture-smoke\"'", timeout=60)
        nomad_server.succeed("nomad job stop -purge telchar-fixture-smoke")
        nomad_server.wait_until_fails("nomad job status telchar-fixture-smoke", timeout=30)
      '';
    };
  nixos-nomad-gateway =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      nomadDerivation = pkgs.writeText "telchar-nomad-gateway.nix" ''
        derivation {
          name = "telchar-nomad-gateway";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "sleep 10; printf nomad > $out" ];
        }
      '';
    in
    harness.mkNomadGatewayTest {
      name = "telchar-nixos-nomad-gateway";
      worker = nomadWorker;
      testScript = ''
        stock_client.succeed("cp ${nomadDerivation} /tmp/telchar-nomad-gateway.nix")
        derivation_path = stock_client.succeed("nix-instantiate /tmp/telchar-nomad-gateway.nix").strip()
        stock_client.succeed("ln -s '" + derivation_path + "' /tmp/nomad-drv-root")
        derivation_export = stock_client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        build = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' '" + derivation_path + "^*'"
        stock_client.succeed("(" + build + " > /tmp/nomad-build-first.out 2>&1) & echo $! > /tmp/nomad-build-first.pid")
        nomad_server.wait_until_succeeds("nomad job status -namespace telchar -json | ${pkgs.jq}/bin/jq -e 'length == 1'", timeout=60)
        job_id = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select backend_execution_id from shared_builds where derivation_path = '" + derivation_path + "'\"").strip()
        assert job_id.startswith("telchar-gateway-")
        nomad_server.succeed("nomad job status -namespace telchar '" + job_id + "'")
        nomad_server.succeed("nomad job inspect -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e '.Namespace == \"telchar\" and .Type == \"batch\" and .Meta.telchar_backend == \"nomad-primary\" and .Meta.telchar_system == \"${pkgs.stdenv.hostPlatform.system}\"'")
        nomad_server.wait_until_succeeds("nomad job allocs -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].ClientStatus == \"running\"'", timeout=60)
        stock_client.succeed("(" + build + " > /tmp/nomad-build-follower.out 2>&1) & echo $! > /tmp/nomad-build-follower.pid")
        stock_client.succeed("kill $(cat /tmp/nomad-build-first.pid)")
        nomad_server.wait_until_succeeds("nomad job allocs -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].ClientStatus == \"complete\"'", timeout=60)
        gateway.wait_until_succeeds("sudo -u postgres psql -d telchar-ingress -Atc \"select state from shared_builds where derivation_path = '" + derivation_path + "'\" | grep -qx succeeded", timeout=60)
        output_path = stock_client.succeed("nix-store -q --outputs '" + derivation_path + "'").strip()
        gateway.succeed("nix-store --verify-path '" + output_path + "' && test \"$(cat '" + output_path + "')\" = nomad")
        stock_client.wait_until_fails("kill -0 $(cat /tmp/nomad-build-follower.pid)", timeout=60)
        stock_client.succeed("grep -Fqx '" + output_path + "' /tmp/nomad-build-follower.out || { cat /tmp/nomad-build-follower.out >&2; exit 1; }")
        stock_client.succeed(build + " > /tmp/nomad-build-reused.out 2>&1 || { cat /tmp/nomad-build-reused.out >&2; exit 1; }")
        stock_client.succeed("grep -Fqx '" + output_path + "' /tmp/nomad-build-reused.out")
        nomad_server.succeed("test $(nomad job status -namespace telchar -json | ${pkgs.jq}/bin/jq 'length') -eq 1")
        nomad_server.succeed("nomad job stop -namespace telchar -purge '" + job_id + "'")
        nomad_server.wait_until_fails("nomad job status -namespace telchar '" + job_id + "'", timeout=30)
      '';
    };
  nixos-static-ssh-fixture =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkStaticSshFixtureTest {
      name = "telchar-nixos-static-ssh-fixture";
      testScript = ''
        start_all()
      '';
    };
  nixos-static-ssh-build =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      remoteOnlyDerivation = pkgs.writeText "telchar-static-ssh-build.nix" ''
        derivation {
          name = "telchar-static-ssh-build";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf 'static-ssh-build-log\\n' >&2; printf static-ssh-source > $out" ];
        }
      '';
    in
    harness.mkStaticSshBuildTest {
      name = "telchar-nixos-static-ssh-build";
      testScript = ''
        stock_client.succeed("cp ${remoteOnlyDerivation} /tmp/static-ssh-build.nix")
        derivation_path = stock_client.succeed("nix-instantiate /tmp/static-ssh-build.nix").strip()
        gateway.succeed("test \"$(nix-instantiate ${remoteOnlyDerivation})\" = '" + derivation_path + "'")
        stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 20 nix --extra-experimental-features nix-command build --no-link --print-build-logs --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway x86_64-linux' --file /tmp/static-ssh-build.nix > /tmp/static-ssh-build.out 2>&1 || { cat /tmp/static-ssh-build.out >&2; exit 1; }")
        output_path = stock_client.succeed("tail -n 1 /tmp/static-ssh-build.out").strip()
        stock_client.succeed("test \"$(cat " + output_path + ")\" = static-ssh-source")
        builder.succeed("grep -Eq '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence")
        gateway.succeed("nix-store --verify-path '" + output_path + "'")
      '';
    };
  nixos-static-ssh-gateway =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      gatewayDerivations = pkgs.writeText "telchar-static-ssh-gateway.nix" ''
        let
          make = name: feature: derivation {
            inherit name;
            system = builtins.currentSystem;
            builder = builtins.storePath "${pkgs.runtimeShell}";
            args = [ "-c" "printf '%s-start\\n' \"$name\" >&2; ${pkgs.coreutils}/bin/sleep 2; printf '%s' \"$name\" > $out" ];
            requiredSystemFeatures = feature;
          };
        in
        {
          shared = make "telchar-static-ssh-shared" [ ];
          first = make "telchar-static-ssh-first" [ "primary" ];
          second = make "telchar-static-ssh-second" [ "secondary" ];
        }
      '';
    in
    harness.mkStaticSshGatewayTest {
      name = "telchar-nixos-static-ssh-gateway";
      testScript = ''
        stock_client.succeed("cp ${gatewayDerivations} /tmp/telchar-static-ssh-gateway.nix")
        shared_path = stock_client.succeed("nix-instantiate /tmp/telchar-static-ssh-gateway.nix -A shared").strip()
        stock_client.succeed("ln -s '" + shared_path + "' /tmp/shared-drv-root")
        ssh_options = "NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null'"
        shared_export = stock_client.succeed("nix-store --export '" + shared_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + shared_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        build = "HOME=/root NIX_CONFIG='substituters =' " + ssh_options + " nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 2 1 primary,secondary'"
        status, output = stock_client.execute("(" + build + " '" + shared_path + "^*' > /tmp/shared-a.out 2>&1) & a=$!; (" + build + " '" + shared_path + "^*' > /tmp/shared-b.out 2>&1) & b=$!; status=0; wait $a || status=1; wait $b || status=1; test $status -eq 0 || { cat /tmp/shared-a.out /tmp/shared-b.out >&2; exit 1; }")
        if status != 0:
            raise Exception("concurrent shared builds failed: " + output)
        shared_a = stock_client.succeed("nix-store -q --outputs '" + shared_path + "'").strip()
        shared_b = shared_a
        assert shared_a == shared_b
        stock_client.succeed("test \"$(cat '" + shared_a + "')\" = telchar-static-ssh-shared")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_build_attempts join shared_builds using (derivation_path) where expected_outputs::text like '%" + shared_a + "%'\" | grep -qx 1")
        first_drv = stock_client.succeed("nix-instantiate /tmp/telchar-static-ssh-gateway.nix -A first").strip()
        second_drv = stock_client.succeed("nix-instantiate /tmp/telchar-static-ssh-gateway.nix -A second").strip()
        distinct_export = stock_client.succeed("nix-store --export '" + first_drv + "' '" + second_drv + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + distinct_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        primary_connections = int(builder_primary.succeed("grep -c '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence").strip())
        secondary_connections = int(builder_secondary.succeed("grep -c '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence").strip())
        assert primary_connections >= 1
        assert secondary_connections >= 1
        stock_client.succeed("(" + build + " '" + first_drv + "^*' > /tmp/first.out 2>&1) & echo $! > /tmp/first.pid; (" + build + " '" + second_drv + "^*' > /tmp/second.out 2>&1) & echo $! > /tmp/second.pid; status=0; wait $(cat /tmp/first.pid) || status=1; wait $(cat /tmp/second.pid) || status=1; test $status -eq 0 || { cat /tmp/first.out /tmp/second.out >&2; exit 1; }")
        first_output = stock_client.succeed("nix-store -q --outputs '" + first_drv + "'").strip()
        second_output = stock_client.succeed("nix-store -q --outputs '" + second_drv + "'").strip()
        stock_client.succeed("test \"$(cat '" + first_output + "')\" = telchar-static-ssh-first")
        stock_client.succeed("test \"$(cat '" + second_output + "')\" = telchar-static-ssh-second")
        builder_primary.succeed("test $(grep -c '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence) -eq " + str(primary_connections + 1))
        builder_secondary.succeed("test $(grep -c '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence) -eq " + str(secondary_connections + 1))
        gateway.succeed("nix-store --verify-path '" + shared_a + "' && nix-store --verify-path '" + first_output + "' && nix-store --verify-path '" + second_output + "'")
      '';
    };
  nixos-gate-3-contract =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      remoteOnlyDerivation = pkgs.writeText "telchar-remote-only-derivation.nix" ''
        let
          source = builtins.toFile "telchar-gate-3-input" "telchar-source-input";
          builder = builtins.storePath "${pkgs.runtimeShell}";
        in
        derivation {
          name = "telchar-gate-3-contract";
          system = builtins.currentSystem;
          inherit builder;
          args = [ "-c" "printf 'telchar-gate-3-build-log\\n' >&2; printf telchar-source-input > $out" ];
          inherit source;
        }
      '';
    in
    harness.mkGate3Test {
      name = "telchar-nixos-gate-3-contract";
      testScript = ''
        start_all()
        otlp_collector.wait_for_open_port(4317)
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_for_unit("sshd.service")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        stock_client.succeed("cp ${remoteOnlyDerivation} /tmp/remote-only.nix")
        stock_client.succeed("test $(timeout -s KILL 20 nix --extra-experimental-features nix-command build --no-link --max-jobs 0 --file /tmp/remote-only.nix > /tmp/local-build.out 2>&1; echo $?) -ne 0")
        stock_client.succeed("grep -Eqi 'unable to start any build|0 local jobs|no enabled build users|cannot build|no machines' /tmp/local-build.out || { cat /tmp/local-build.out >&2; exit 1; }")
        gateway.succeed("mkdir -p /run/telchar-direct-bin /var/lib/telchar-direct-client")
        gateway.succeed("nix --extra-experimental-features nix-command copy --no-check-sigs --to 'local?root=/var/lib/telchar-direct-client' ${pkgs.runtimeShell}")
        gateway.succeed("derivation_path=$(nix-instantiate ${remoteOnlyDerivation}); nix --extra-experimental-features nix-command copy --no-check-sigs --to 'local?root=/var/lib/telchar-direct-client' \"$derivation_path\"")
        gateway.succeed("printf '#!/bin/sh\\nset -eu\\ncase \" $* \" in *\" -O check \"*) exit 1 ;; esac\\nprintf '\"'\"'started\\n'\"'\"'\\nexec sudo -u telchar-ingress env TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock TELCHAR_AUTHENTICATED_KEY=SHA256:direct-stdio ${telchar}/bin/telchar serve-stdio\\n' > /run/telchar-direct-bin/ssh && chmod 755 /run/telchar-direct-bin/ssh")
        gateway.succeed("env PATH=/run/telchar-direct-bin:$PATH NIX_CONFIG='substituters =\nsandbox = false\nbuild-users-group =' timeout -s KILL 60 nix --extra-experimental-features nix-command --store 'local?root=/var/lib/telchar-direct-client' build --no-link --print-build-logs --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-direct x86_64-linux' --file ${remoteOnlyDerivation} > /tmp/direct-build.out 2>&1 || { cat /tmp/direct-build.out >&2; exit 1; }")
        direct_output_path = gateway.succeed("tail -n 1 /tmp/direct-build.out").strip()
        gateway.succeed("test \"$(cat /var/lib/telchar-direct-client" + direct_output_path + ")\" = telchar-source-input")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND released_at IS NULL AND store_path = '" + direct_output_path + "'\" | grep -qx 1")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose IN ('derivation', 'input') AND state = 'active'\" | grep -qx 0")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM request_attachments WHERE state = 'detached' AND detached_at IS NOT NULL\" | grep -qx 1")
        gateway.succeed("direct_output_root=$(find /var/lib/telchar-gc-roots -mindepth 1 -maxdepth 1 -type l -lname '" + direct_output_path + "' -print); test \"$(printf '%s\\n' \"$direct_output_root\" | sed '/^$/d' | wc -l)\" -eq 1")
        stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 60 nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway x86_64-linux' --file /tmp/remote-only.nix > /tmp/remote-build.out 2>&1 || { cat /tmp/remote-build.out >&2; exit 1; }")
        output_path = stock_client.succeed("tail -n 1 /tmp/remote-build.out").strip()
        stock_client.succeed("test \"$(cat " + output_path + ")\" = telchar-source-input")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.query_valid_paths.completed'")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.add_multiple_to_store.completed'")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.build_derivation.admitted'")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.build_derivation.completed'")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'input' AND state = 'released' AND released_at IS NOT NULL\" | grep -qx 12")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'derivation' AND state = 'released' AND released_at IS NOT NULL\" | grep -qx 2")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND released_at IS NULL\" | grep -qx 2")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose IN ('derivation', 'input') AND state = 'active'\" | grep -qx 0")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM request_attachments WHERE state = 'detached' AND detached_at IS NOT NULL\" | grep -qx 2")
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT store_path FROM store_leases WHERE owner_kind = 'request' AND purpose = 'input' AND state = 'released'\" > /tmp/telchar-input-leases")
        gateway.succeed("test \"$(wc -l < /tmp/telchar-input-leases)\" -eq 12")
        gateway.succeed("while IFS= read -r released_input; do test -e \"$released_input\"; done < /tmp/telchar-input-leases")
        gateway.succeed("grep -Eq '/nix/store/[0-9a-df-np-sv-z]{32}-telchar-gate-3-input$' /tmp/telchar-input-leases")
        gateway.succeed("output_roots=$(find /var/lib/telchar-gc-roots -mindepth 1 -maxdepth 1 -type l -lname '" + output_path + "' -print); test \"$(printf '%s\\n' \"$output_roots\" | sed '/^$/d' | wc -l)\" -eq 2")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.query_path_info.completed.*valid=true'")
        gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.nar_from_path.completed'")
        gateway.succeed("grep -q '^authenticated_key=SHA256:' /run/telchar/forced-command-evidence")
      '';
    };
  nixos-restart-reconciliation =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      recoveryOutput = telchar;
      seedSql = pkgs.writeText "telchar-restart-reconciliation.sql" ''
        INSERT INTO build_requests (request_id, derivation_path, system, queue_state, queued_at, audit_subject, quota_subject) VALUES
          ('queued-recovery', '/nix/store/11111111111111111111111111111111-queued.drv', '${system}', 'queued', transaction_timestamp(), 'test-audit', 'test-quota'),
          ('running-recovery', '/nix/store/22222222222222222222222222222222-running.drv', '${system}', 'running', transaction_timestamp(), 'test-audit', 'test-quota'),
          ('collecting-recovery', '/nix/store/33333333333333333333333333333333-collecting.drv', '${system}', 'collecting', transaction_timestamp(), 'test-audit', 'test-quota');

        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at) VALUES
          ('queued-derivation', 'request', 'queued-recovery', '/nix/store/11111111111111111111111111111111-queued.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL),
          ('queued-input', 'request', 'queued-recovery', '/nix/store/44444444444444444444444444444444-input', 'input', 'active', transaction_timestamp(), NULL, NULL),
          ('running-derivation', 'request', 'running-recovery', '/nix/store/22222222222222222222222222222222-running.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL),
          ('collecting-derivation', 'request', 'collecting-recovery', '/nix/store/33333333333333333333333333333333-collecting.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL);

        INSERT INTO execution_attempts (attempt_id, request_id, ordinal, idempotency_key, backend, backend_execution_id, state, created_at, submitted_at, started_at, collecting_at) VALUES
          ('running-attempt', 'running-recovery', 1, 'running-recovery:1', 'local', 'running-backend', 'running', transaction_timestamp(), transaction_timestamp(), transaction_timestamp(), NULL),
          ('collecting-attempt', 'collecting-recovery', 1, 'collecting-recovery:1', 'local', 'collecting-backend', 'collecting', transaction_timestamp(), transaction_timestamp(), transaction_timestamp(), transaction_timestamp());

        INSERT INTO capacity_reservations (reservation_id, attempt_id, phase, quota_subject, units, created_at) VALUES
          ('running-reservation', 'running-attempt', 'running', 'test-quota', 1, transaction_timestamp()),
          ('collecting-reservation', 'collecting-attempt', 'collecting', 'test-quota', 1, transaction_timestamp());

        INSERT INTO local_backend_executions (backend_execution_id, idempotency_key, specification_digest, state, created_at, started_at, completed_at) VALUES
          ('running-backend', 'running-recovery:1', decode(repeat('08', 32), 'hex'), 'running', transaction_timestamp(), transaction_timestamp(), NULL),
          ('collecting-backend', 'collecting-recovery:1', decode(repeat('09', 32), 'hex'), 'succeeded', transaction_timestamp(), transaction_timestamp(), transaction_timestamp());

        INSERT INTO local_backend_execution_results (backend_execution_id, classification, result_metadata, created_at) VALUES
          ('collecting-backend', 'succeeded', jsonb_build_object('status', 'built', 'outputs', jsonb_build_array(jsonb_build_object('name', 'out', 'path', '${recoveryOutput}'))), transaction_timestamp());
      '';
    in
    harness.mkRestartRecoveryTest {
      name = "telchar-nixos-restart-reconciliation";
      testScript = ''
        start_all()
        postgres.wait_for_unit("postgresql.service")
        owner.succeed("systemctl start telchar-recovery-daemon.service")
        owner.wait_for_file("/run/telchar-recovery/daemon.sock")

        replacement.succeed("systemctl start telchar-recovery-daemon.service")
        replacement.wait_until_fails("systemctl is-active --quiet telchar-recovery-daemon.service")
        replacement.succeed("systemctl show telchar-recovery-daemon.service -p Result --value | grep -qx exit-code")
        replacement.succeed("test ! -S /run/telchar-recovery/daemon.sock")
        owner.succeed("test -S /run/telchar-recovery/daemon.sock")
        replacement.succeed("journalctl -u telchar-recovery-daemon.service --no-pager | grep -q database.singleton_ownership.refused")

        postgres.succeed("sudo -u postgres psql -d telchar-recovery -v ON_ERROR_STOP=1 -f ${seedSql}")
        postgres.succeed("systemctl restart postgresql.service")
        postgres.wait_for_unit("postgresql.service")
        owner.wait_until_fails("systemctl is-active --quiet telchar-recovery-daemon.service")
        owner.succeed("test ! -S /run/telchar-recovery/daemon.sock")
        owner.succeed("journalctl -u telchar-recovery-daemon.service --no-pager | grep -q database.singleton_ownership.lost")

        replacement.succeed("systemctl reset-failed telchar-recovery-daemon.service")
        replacement.succeed("systemctl start telchar-recovery-daemon.service")
        replacement.wait_for_file("/run/telchar-recovery/daemon.sock")

        postgres.wait_until_succeeds("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT queue_state FROM build_requests WHERE request_id = 'collecting-recovery'\" | grep -qx completed")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM build_requests WHERE request_id = 'queued-recovery' AND queue_state = 'queued'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM execution_attempts WHERE attempt_id = 'running-attempt' AND idempotency_key = 'running-recovery:1' AND backend_execution_id = 'running-backend' AND state = 'running'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_executions WHERE backend_execution_id = 'running-backend' AND idempotency_key = 'running-recovery:1' AND state = 'running'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_executions\" | grep -qx 2")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM execution_attempts\" | grep -qx 2")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_execution_results\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM execution_outcomes WHERE attempt_id = 'collecting-attempt' AND classification = 'succeeded'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM store_leases WHERE owner_id = 'collecting-recovery' AND purpose = 'output' AND state = 'active' AND store_path = '${recoveryOutput}'\" | grep -qx 1")
        replacement.succeed("test \"$(find /var/lib/telchar-recovery-roots -mindepth 1 -maxdepth 1 -type l -lname '${recoveryOutput}' | wc -l)\" -eq 1")
        replacement.succeed("journalctl -u telchar-recovery-daemon.service --no-pager | grep -q database.singleton_ownership.acquired")
      '';
    };
  nixos-artifacts =
    let
      harness = import ../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
    in
    harness.mkTest {
      name = "telchar-nixos-artifacts";
      includeCollector = true;
      testScript = ''
        start_all()
        otlp_collector.wait_for_open_port(4317)
        gateway.succeed("systemctl restart telchar.service")
        gateway.wait_for_unit("telchar.service")
        gateway.fail("systemctl start telchar-artifacts-failure.service")
        gateway.succeed("systemctl start telchar-artifacts.service")
        gateway.wait_for_unit("telchar-artifacts.service")
        gateway.succeed("test -s /var/lib/telchar-artifacts/journal.log")
        gateway.succeed("test -s /var/lib/telchar-artifacts/machine-state.json")
        gateway.succeed("grep -q '^Id=telchar-artifacts-failure.service$' /var/lib/telchar-artifacts/machine-state.json")
        gateway.succeed("grep -q '^ActiveState=failed$' /var/lib/telchar-artifacts/machine-state.json")
        gateway.succeed("grep -q '^Result=exit-code$' /var/lib/telchar-artifacts/machine-state.json")
        gateway.succeed("! grep -q not-for-artifacts /var/lib/telchar-artifacts/journal.log /var/lib/telchar-artifacts/machine-state.json")
        otlp_collector.wait_until_succeeds("test -s /var/lib/telchar-otlp/records.json")
        otlp_collector.succeed("mkdir -p /var/lib/telchar-artifacts && cp /var/lib/telchar-otlp/records.json /var/lib/telchar-artifacts/otlp-records.json")
        otlp_collector.succeed("test -s /var/lib/telchar-artifacts/otlp-records.json")
      '';
    };
}
