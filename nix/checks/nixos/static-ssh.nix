# Defines static SSH fixture, backend, and gateway checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
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
