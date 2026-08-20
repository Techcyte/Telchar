# Defines gate-three contracts and durable restart reconciliation checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-gate-3-contract =
    let
      harness = import ../../../tests/nixos/lib.nix {
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
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      recoveryOutput = telchar;
      seedSql = pkgs.writeText "telchar-restart-reconciliation.sql" ''
        INSERT INTO build_requests (request_id, derivation_path, system, audit_subject, quota_subject) VALUES
          ('queued-recovery', '/nix/store/11111111111111111111111111111111-queued.drv', '${system}', 'test-audit', 'test-quota'),
          ('running-recovery', '/nix/store/22222222222222222222222222222222-running.drv', '${system}', 'test-audit', 'test-quota'),
          ('collecting-recovery', '/nix/store/33333333333333333333333333333333-collecting.drv', '${system}', 'test-audit', 'test-quota');

        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, created_at, released_at, expires_at, nar_size) VALUES
          ('queued-derivation', 'request', 'queued-recovery', '/nix/store/11111111111111111111111111111111-queued.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL, 1),
          ('queued-input', 'request', 'queued-recovery', '/nix/store/44444444444444444444444444444444-input', 'input', 'active', transaction_timestamp(), NULL, NULL, 1),
          ('running-derivation', 'request', 'running-recovery', '/nix/store/22222222222222222222222222222222-running.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL, 1),
          ('collecting-derivation', 'request', 'collecting-recovery', '/nix/store/33333333333333333333333333333333-collecting.drv', 'derivation', 'active', transaction_timestamp(), NULL, NULL, 1);

        INSERT INTO shared_builds (
          derivation_path, request_digest, state, backend_name, backend_kind,
          execution_recovery, cancellation, log_recovery, backend_execution_id,
          expected_outputs, created_at, started_at, collecting_at,
          quota_subject, queue_position, queued_at, build_request
        ) VALUES
          ('/nix/store/11111111111111111111111111111111-queued.drv', decode(repeat('01', 32), 'hex'), 'claimed', 'local', 'local', 'output-only', 'connection-bound', 'live-only', NULL, ARRAY['/nix/store/55555555555555555555555555555555-queued-output'], transaction_timestamp(), NULL, NULL, 'test-quota', nextval('shared_build_queue_position_seq'), transaction_timestamp(), NULL),
          ('/nix/store/22222222222222222222222222222222-running.drv', decode(repeat('02', 32), 'hex'), 'running', 'local', 'local', 'output-only', 'connection-bound', 'live-only', 'running-backend', ARRAY['/nix/store/66666666666666666666666666666666-running-output'], transaction_timestamp(), transaction_timestamp(), NULL, NULL, NULL, NULL, NULL),
          ('/nix/store/33333333333333333333333333333333-collecting.drv', decode(repeat('03', 32), 'hex'), 'collecting', 'local', 'local', 'output-only', 'connection-bound', 'live-only', 'collecting-backend', ARRAY['${recoveryOutput}'], transaction_timestamp(), transaction_timestamp(), transaction_timestamp(), NULL, NULL, NULL, NULL);

        INSERT INTO shared_build_attempts (derivation_path, ordinal, backend_name, backend_kind, backend_execution_id, state, created_at, started_at, collecting_at) VALUES
          ('/nix/store/22222222222222222222222222222222-running.drv', 1, 'local', 'local', 'running-backend', 'running', transaction_timestamp(), transaction_timestamp(), NULL),
          ('/nix/store/33333333333333333333333333333333-collecting.drv', 1, 'local', 'local', 'collecting-backend', 'collecting', transaction_timestamp(), transaction_timestamp(), transaction_timestamp());

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
        owner.succeed("systemctl kill --signal=SIGKILL telchar-recovery-daemon.service")
        owner.wait_until_fails("systemctl is-active --quiet telchar-recovery-daemon.service")
        owner.succeed("rm -f /run/telchar-recovery/daemon.sock")

        replacement.wait_until_succeeds("systemctl reset-failed telchar-recovery-daemon.service; systemctl start telchar-recovery-daemon.service; test -S /run/telchar-recovery/daemon.sock", timeout=10)
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT generation >= 2 AND lease_expires_at > clock_timestamp() FROM singleton_ownership WHERE owner_kind = 'daemon'\" | grep -qx t")

        postgres.wait_until_succeeds("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT state FROM shared_builds WHERE derivation_path = '/nix/store/33333333333333333333333333333333-collecting.drv'\" | grep -qx succeeded")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM shared_builds WHERE derivation_path = '/nix/store/11111111111111111111111111111111-queued.drv'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM shared_build_attempts WHERE derivation_path = '/nix/store/22222222222222222222222222222222-running.drv' AND backend_execution_id = 'running-backend' AND state = 'failed'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_executions WHERE backend_execution_id = 'running-backend' AND state = 'running'\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_executions\" | grep -qx 2")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM shared_build_attempts\" | grep -qx 2")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM local_backend_execution_results\" | grep -qx 1")
        postgres.succeed("sudo -u postgres psql -d telchar-recovery -Atc \"SELECT count(*) FROM shared_build_attempt_outcomes WHERE classification = 'restart-recovery-failed'\" | grep -qx 1")
        replacement.succeed("journalctl -u telchar-recovery-daemon.service --no-pager | grep -q database.singleton_ownership.acquired")
      '';
    };
}
