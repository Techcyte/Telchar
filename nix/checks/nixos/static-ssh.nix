# Defines static SSH fixture, backend, and gateway checks.
{
  pkgs,
  telchar,
  ...
}:
{
  nixos-static-ssh-fixture =
    let
      harness = import ../../../tests/nixos/lib.nix {
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
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      remoteOnlyDerivation = pkgs.writeText "telchar-static-ssh-build.nix" ''
        let
          dependency = builtins.toFile "telchar-cold-dependency" "static-ssh-source";
          candidates = builtins.genList (index:
            builtins.toFile "telchar-cold-parent-''${toString index}" dependency
          ) 64;
          parent = builtins.head (builtins.filter (path: builtins.lessThan path dependency) candidates);
        in derivation {
          name = "telchar-static-ssh-build";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          input = parent;
          args = [ "-c" "printf 'static-ssh-build-log\\n' >&2; read dependency < $input || test -n \"$dependency\"; read content < $dependency || test -n \"$content\"; printf '%s' \"$content\" > $out" ];
        }
      '';
    in
    harness.mkStaticSshBuildTest {
      name = "telchar-nixos-static-ssh-build";
      testScript = ''
        stock_client.succeed("cp ${remoteOnlyDerivation} /tmp/static-ssh-build.nix")
        derivation_path = stock_client.succeed("nix-instantiate /tmp/static-ssh-build.nix").strip()
        gateway.succeed("test \"$(nix-instantiate ${remoteOnlyDerivation})\" = '" + derivation_path + "'")
        inputs = stock_client.succeed("nix-store -q --references '" + derivation_path + "'").splitlines()
        parent = next(path for path in inputs if "-telchar-cold-parent-" in path)
        dependency = stock_client.succeed("cat '" + parent + "'").strip()
        assert parent < dependency, "fixture must stage referrer before dependency in lexical order"
        for path in [parent, dependency]:
            builder.fail("nix-store --check-validity '" + path + "'")
        stock_client.succeed("HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 20 nix --extra-experimental-features nix-command build --no-link --print-build-logs --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway x86_64-linux' --file /tmp/static-ssh-build.nix > /tmp/static-ssh-build.out 2>&1 || { cat /tmp/static-ssh-build.out >&2; exit 1; }")
        stock_client.succeed("grep -Fq 'static-ssh-build-log' /tmp/static-ssh-build.out")
        output_path = stock_client.succeed("tail -n 1 /tmp/static-ssh-build.out").strip()
        stock_client.succeed("test \"$(cat " + output_path + ")\" = static-ssh-source")
        builder.succeed("grep -Eq '^original_command=nix-daemon --stdio' /var/lib/telchar-builder/forced-command-evidence")
        gateway.succeed("nix-store --verify-path '" + output_path + "'")
      '';
    };
  nixos-direct-store-dependencies =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs telchar;
      };
      derivations = pkgs.writeText "telchar-direct-store-dependencies.nix" ''
        { name ? "direct", fail ? false }:
        let
          make = suffix: inputs: command: derivation ({
            name = "telchar-''${name}-''${suffix}";
            system = "aarch64-linux";
            builder = builtins.storePath "${pkgs.runtimeShell}";
            args = [ "-c" command ];
          } // inputs);
          leaf = make "leaf" {} (if fail then "exit 7" else "printf leaf > $out");
          middle = make "middle" { inherit leaf; } "read value < $leaf; printf '%s-middle' $value > $out";
        in make "root" { inherit leaf middle; } "read value < $middle; printf '%s-root' $value > $out"
      '';
    in
    harness.mkStaticSshBuildTest {
      name = "telchar-nixos-direct-store-dependencies";
      # The builder accepts an ARM platform label using x86 executables.
      builderSystem = "aarch64-linux";
      testScript = ''
        import shlex
        ssh = "HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' "
        store = "ssh-ng://telchar-ingress@gateway"
        nix = ssh + "timeout 90 nix --extra-experimental-features nix-command --option substituters \"\" "
        roots = {}
        for mode in ["direct", "builders", "failed"]:
            drv = stock_client.succeed("nix-instantiate ${derivations} --argstr name " + mode + " --arg fail " + ("true" if mode == "failed" else "false")).strip()
            roots[mode] = drv
            graph = stock_client.succeed("nix-store -qR " + shlex.quote(drv)).splitlines()
            drvs = [path for path in graph if path.endswith(".drv")]
            assert len(drvs) == 3, drvs
            outputs = {path: stock_client.succeed("nix-store -q --outputs " + shlex.quote(path)).strip() for path in drvs}
            for output in outputs.values():
                gateway.fail("nix-store --check-validity " + shlex.quote(output))
                builder.fail("nix-store --check-validity " + shlex.quote(output))
            if mode != "builders":
                stock_client.succeed(nix + "copy --to " + store + " " + shlex.quote(drv))
            command = nix + "build --no-link --print-out-paths "
            if mode == "builders":
                command += "--max-jobs 0 --builders '" + store + " aarch64-linux' "
            else:
                command += "--store " + store + " "
            command += shlex.quote(drv + "^*") + " > /tmp/" + mode + ".out 2>/tmp/" + mode + ".err"
            if mode == "failed":
                stock_client.fail(command)
                for path, output in outputs.items():
                    gateway.fail("nix-store --check-validity " + shlex.quote(output))
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_builds where derivation_path like '%telchar-failed-leaf.drv' and state = 'failed'\" | grep -qx 1")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_builds where derivation_path like '%telchar-failed-root.drv' or derivation_path like '%telchar-failed-middle.drv'\" | grep -qx 0")
            else:
                stock_client.succeed(command + " || { cat /tmp/" + mode + ".err >&2; exit 1; }")
                root = outputs[drv]
                gateway.succeed("test \"$(cat " + shlex.quote(root) + ")\" = leaf-middle-root")
                for path, output in outputs.items():
                    gateway.succeed("nix-store --verify-path " + shlex.quote(output))
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_build_attempts join shared_builds using (derivation_path) where derivation_path like '%telchar-" + mode + "-%' and state = 'succeeded' and shared_builds.backend_kind = 'static-ssh'\" | grep -qx 3")
        gateway.succeed("systemctl restart telchar-daemon.service")
        gateway.wait_for_unit("telchar-daemon.service")
        stock_client.succeed(nix + "build --store " + store + " --no-link " + shlex.quote(roots["direct"] + "^*") + " >/tmp/reuse.out 2>/tmp/reuse.err || { cat /tmp/reuse.err >&2; exit 1; }")
      '';
    };
  nixos-static-ssh-gateway =
    let
      harness = import ../../../tests/nixos/lib.nix {
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
}
