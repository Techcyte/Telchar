# Defines Nomad fixture and gateway checks.
{
  pkgs,
  telchar,
  nomadWorker,
  ...
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
      callbackProbe = import ../../../tests/nixos/callback-probe.nix { inherit pkgs; };
      nomadDerivation = pkgs.writeText "telchar-nomad-gateway.nix" ''
        derivation {
          name = "telchar-nomad-gateway";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          sleepPackage = builtins.storePath "${pkgs.coreutils}";
          textInput = builtins.toFile "telchar-nomad-text-input" "reference: ''${builtins.toFile "telchar-nomad-text-reference" "nomad reference"}";
          args = [ "-c" "test -s \"$textInput\" && ${pkgs.coreutils}/bin/sleep 10 && printf nomad > \"$out\"" ];
        }
      '';
      failingDerivation = pkgs.writeText "telchar-nomad-failure.nix" ''
        derivation {
          name = "telchar-nomad-failure";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          sleepPackage = builtins.storePath "${pkgs.coreutils}";
          args = [ "-c" "$sleepPackage/bin/sleep 10; echo nomad-exit-three >&2; exit 3" ];
        }
      '';
    in
    harness.mkNomadGatewayTest {
      name = "telchar-nixos-nomad-gateway";
      worker = nomadWorker;
      testScript = ''
        # Unauthenticated peers must be rejected without preventing an authenticated build.
        stock_client.succeed("${pkgs.python3}/bin/python3 ${callbackProbe} invalid")
        stock_client.succeed("${pkgs.python3}/bin/python3 ${callbackProbe} missing")
        stock_client.succeed("cp ${nomadDerivation} /tmp/telchar-nomad-gateway.nix")
        derivation_path = stock_client.succeed("nix-instantiate /tmp/telchar-nomad-gateway.nix").strip()
        stock_client.succeed("ln -s '" + derivation_path + "' /tmp/nomad-drv-root")
        import shlex
        text_input = stock_client.succeed("nix-instantiate --eval --strict --json --expr '(import /tmp/telchar-nomad-gateway.nix).textInput'").strip()
        text_input = json.loads(text_input)
        # BuildDerivation sends a derivation value, not its .drv store path.
        source_info = json.loads(stock_client.succeed("nix --extra-experimental-features nix-command path-info --json --json-format 1 --recursive " + shlex.quote(text_input)))
        fresh_paths = list(source_info)
        assert len(fresh_paths) == 2, source_info
        for path in fresh_paths:
            gateway.succeed("test ! -e " + shlex.quote(path))
            gateway.fail("nix-store --check-validity " + shlex.quote(path))
            assert source_info[path]["ca"].startswith("text:sha256:"), source_info[path]
        assert len(source_info[text_input]["references"]) == 1, source_info[text_input]
        build = "HOME=/root NIX_CONFIG='substituters =\nbuilders =\npost-build-hook =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' '" + derivation_path + "^*'"
        stock_client.succeed("(" + build + " > /tmp/nomad-build-first.out 2>&1) & echo $! > /tmp/nomad-build-first.pid")
        nomad_server.wait_until_succeeds("${pkgs.curl}/bin/curl --fail --silent --show-error 'http://127.0.0.1:4646/v1/jobs?namespace=telchar' | ${pkgs.jq}/bin/jq -e 'length == 1'", timeout=60)
        job_id = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select backend_execution_id from shared_builds where derivation_path = '" + derivation_path + "'\"").strip()
        assert job_id.startswith("telchar-gateway-")
        nomad_server.succeed("nomad job status -namespace telchar '" + job_id + "'")
        nomad_server.succeed("nomad job inspect -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e '.Namespace == \"telchar\" and .Type == \"batch\" and .Meta.telchar_backend == \"nomad-primary\" and .Meta.telchar_system == \"${pkgs.stdenv.hostPlatform.system}\"'")
        nomad_server.wait_until_succeeds("nomad job allocs -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].ClientStatus == \"running\"'", timeout=60)
        stock_client.succeed("(" + build + " > /tmp/nomad-build-follower.out 2>&1) & echo $! > /tmp/nomad-build-follower.pid")
        stock_client.succeed("kill $(cat /tmp/nomad-build-first.pid)")
        nomad_server.wait_until_succeeds("nomad job allocs -namespace telchar -json '" + job_id + "' | ${pkgs.jq}/bin/jq -e 'length == 1 and .[0].ClientStatus == \"complete\"'", timeout=60)
        gateway.wait_until_succeeds("sudo -u postgres psql -d telchar-ingress -Atc \"select state from shared_builds where derivation_path = '" + derivation_path + "'\" | grep -qx succeeded", timeout=60)
        destination_info = json.loads(gateway.succeed("nix --extra-experimental-features nix-command path-info --json --json-format 1 " + " ".join(map(shlex.quote, fresh_paths))))
        for path in fresh_paths:
            for field in ["narHash", "narSize", "references", "ca"]:
                assert source_info[path][field] == destination_info[path][field], (path, field)
            assert stock_client.succeed("cat " + shlex.quote(path)) == gateway.succeed("cat " + shlex.quote(path)), path
            gateway.succeed("nix-store --verify-path " + shlex.quote(path))
        output_path = stock_client.succeed("nix-store -q --outputs '" + derivation_path + "'").strip()
        gateway.succeed("nix-store --verify-path '" + output_path + "' && test \"$(cat '" + output_path + "')\" = nomad")
        stock_client.wait_until_fails("kill -0 $(cat /tmp/nomad-build-follower.pid)", timeout=60)
        stock_client.succeed("grep -Fqx '" + output_path + "' /tmp/nomad-build-follower.out || { cat /tmp/nomad-build-follower.out >&2; exit 1; }")
        stock_client.succeed(build + " > /tmp/nomad-build-reused.out 2>&1 || { cat /tmp/nomad-build-reused.out >&2; exit 1; }")
        stock_client.succeed("grep -Fqx '" + output_path + "' /tmp/nomad-build-reused.out")
        nomad_server.succeed("${pkgs.curl}/bin/curl --fail --silent --show-error 'http://127.0.0.1:4646/v1/jobs?namespace=telchar' | ${pkgs.jq}/bin/jq -e 'length == 1'")
        producer_node = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select location from output_producers where store_path = '" + output_path + "'\"").strip()
        assert producer_node, "validated Nomad output must retain exact allocation NodeID"
        consumer_expr = 'derivation { name = "telchar-locality-consumer"; system = builtins.currentSystem; builder = builtins.storePath "${pkgs.runtimeShell}"; tools = builtins.storePath "${pkgs.coreutils}"; dependency = builtins.storePath ' + output_path + '; args = [ "-c" "$tools/bin/cat $dependency > $out" ]; }'
        stock_client.succeed("printf '%s' " + shlex.quote(consumer_expr) + " > /tmp/locality-consumer.nix")
        consumer_drv = stock_client.succeed("nix-instantiate /tmp/locality-consumer.nix").strip()
        consumer_output = stock_client.succeed(build.replace(derivation_path, consumer_drv)).strip()
        gateway.succeed("test \"$(cat " + shlex.quote(consumer_output) + ")\" = nomad")
        consumer_job = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select backend_execution_id from shared_builds where derivation_path = '" + consumer_drv + "'\"").strip()
        consumer_spec = json.loads(nomad_server.succeed("nomad job inspect -namespace telchar -json " + shlex.quote(consumer_job)))
        assert consumer_spec["Affinities"] == [{"LTarget": "$" + "{node.unique.id}", "Operand": "=", "RTarget": producer_node, "Weight": 50}], consumer_spec
        assert not any(item.get("LTarget") == "$" + "{node.unique.id}" for item in (consumer_spec.get("Constraints") or [])), consumer_spec
        # A sole request and four joined stock clients must see the actual Nix
        # failure, not a transfer error. A later request executes a new attempt.
        failed_drv = stock_client.succeed("nix-instantiate ${failingDerivation}").strip()
        failed_build = build.replace(derivation_path, failed_drv)
        failed_export = stock_client.succeed("nix-store --export '" + failed_drv + "' | base64 -w0").strip()
        gateway.succeed("printf '%s' '" + failed_export + "' | base64 -d | nix-store --import >/dev/null")
        # Direct ssh-ng store clients avoid local Nix coalescing before the gateway.
        keyed_failure = "HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 90 nix --extra-experimental-features nix-command build --no-link --print-build-logs --store ssh-ng://telchar-ingress@gateway '" + failed_drv + "^*'"
        for i in range(4):
            # The driver enables errexit; capture expected failure before writing status.
            stock_client.succeed("(status=0; " + keyed_failure + " > /tmp/failed-" + str(i) + ".log 2>&1 || status=$?; echo $status > /tmp/failed-" + str(i) + ".status) </dev/null >/dev/null 2>&1 &")
        try:
            for i in range(4):
                stock_client.wait_for_file("/tmp/failed-" + str(i) + ".status")
                status = int(stock_client.succeed("cat /tmp/failed-" + str(i) + ".status").strip())
                assert status not in (0, 124, 137), status
                stock_client.succeed("grep -Fq 'exit code 3' /tmp/failed-" + str(i) + ".log")
                stock_client.succeed("grep -Fq nomad-exit-three /tmp/failed-" + str(i) + ".log")
                stock_client.fail("grep -E 'Nomad build transfer failed|unexpected end-of-file|Connection reset' /tmp/failed-" + str(i) + ".log")
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from request_attachments a join build_requests r on a.request_id = r.request_id where r.derivation_path = '" + failed_drv + "'\" | grep -qx 4")
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_build_attempts where derivation_path = '" + failed_drv + "'\" | grep -qx 1")
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select result_metadata->'nix_failure'->>'status' from shared_builds where derivation_path = '" + failed_drv + "'\" | grep -qx 3")
            stock_client.fail(failed_build + " > /tmp/failed-retry.log 2>&1")
            stock_client.succeed("grep -Fq 'exit code 3' /tmp/failed-retry.log")
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_build_attempts where derivation_path = '" + failed_drv + "'\" | grep -qx 2")
        finally:
            print(stock_client.succeed("for f in /tmp/failed-*.log /tmp/failed-*.status; do if test -f \"$f\"; then echo \"=== $f ===\"; cat \"$f\"; fi; done"))
        nomad_server.succeed("nomad job stop -namespace telchar -purge '" + job_id + "'")
        nomad_server.wait_until_fails("nomad job status -namespace telchar '" + job_id + "'", timeout=30)
      '';
    };
}
