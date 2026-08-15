# Defines Nomad fixture and gateway checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-nomad-fixture =
    let
      harness = import ../../../tests/nixos/lib.nix {
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
      harness = import ../../../tests/nixos/lib.nix {
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
}
