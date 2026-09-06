# Verifies worker telemetry and client build logs using real Nomad, Nix and OTLP services.
{
  pkgs,
  telchar,
  nomadWorker,
  ...
}:
let
  harness = import ../../../tests/nixos/lib.nix { inherit pkgs telchar; };
  worker = pkgs.writeShellScriptBin "telchar-nomad-worker" ''
    export RUST_LOG="info,telchar_nomad_worker=$(cat /run/worker-level)"
    export OTEL_EXPORTER_OTLP_PROTOCOL="$(cat /run/worker-protocol)"
    export OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:$(cat /run/worker-port)"
    exec ${nomadWorker}/bin/telchar-nomad-worker
  '';
  collectorConfig = pkgs.writeText "worker-collector.yaml" ''
    receivers:
      otlp:
        protocols:
          grpc:
            endpoint: 127.0.0.1:4317
          http:
            endpoint: 127.0.0.1:4318
    exporters:
      file:
        path: /tmp/worker-telemetry.json
    service:
      pipelines:
        traces:
          receivers: [otlp]
          exporters: [file]
        metrics:
          receivers: [otlp]
          exporters: [file]
        logs:
          receivers: [otlp]
          exporters: [file]
  '';
in
harness.mkNomadGatewayTest {
  name = "telchar-nixos-worker-telemetry";
  inherit worker;
  testScript = ''
    import json
    import shlex
    import uuid

    nomad_client.succeed("systemd-run --unit=worker-collector ${pkgs.opentelemetry-collector}/bin/otelcol --config ${collectorConfig}")
    nomad_client.wait_for_unit("worker-collector.service")
    stock_client.succeed("ssh-keyscan -t ed25519 gateway > /root/.ssh/known_hosts 2>/dev/null")
    for level, protocol, port in [("info", "grpc", 4317), ("debug", "http/protobuf", 4318), ("info", "grpc", 9)]:
        nomad_client.succeed("printf %s " + shlex.quote(level) + " > /run/worker-level; printf %s " + shlex.quote(protocol) + " > /run/worker-protocol; echo " + str(port) + " > /run/worker-port")
        nonce = uuid.uuid4().hex
        script = "echo WORKER_BUILD_BEGIN >&2; $tools/bin/sleep 12; echo WORKER_BUILD_END >&2; printf " + nonce + " > $out"
        expr = 'derivation { name = "telemetry-' + nonce + '"; system = "${pkgs.stdenv.hostPlatform.system}"; builder = builtins.storePath "${pkgs.runtimeShell}"; tools = builtins.storePath "${pkgs.coreutils}"; args = [ "-c" ' + json.dumps(script) + ' ]; }'
        drv = stock_client.succeed("nix-instantiate --expr " + shlex.quote(expr)).strip()
        output = stock_client.succeed("nix-store -q --outputs " + shlex.quote(drv)).strip()
        gateway.succeed("test ! -e " + shlex.quote(output))
        data = stock_client.succeed("nix-store --export " + shlex.quote(drv) + " | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf %s " + shlex.quote(data) + " | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        build = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes' nix --extra-experimental-features nix-command build -L --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${pkgs.stdenv.hostPlatform.system} - 1 1' " + shlex.quote(drv + "^*")
        client_log = stock_client.succeed(build + " 2>&1", timeout=120)
        for marker in ["WORKER_BUILD_BEGIN", "WORKER_BUILD_END"]:
            assert marker in client_log, client_log
        gateway.succeed("nix-store --verify-path " + shlex.quote(output) + "; test $(cat " + shlex.quote(output) + ") = " + nonce)
        job = gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc " + shlex.quote("select backend_execution_id from shared_builds where derivation_path = '" + drv + "'")).strip()
        allocations = json.loads(nomad_server.succeed("nomad job allocs -namespace telchar -json " + shlex.quote(job)))
        assert len(allocations) == 1, allocations
        allocation = allocations[0]["ID"]
        nomad_server.wait_until_succeeds("nomad alloc status -namespace telchar -json " + shlex.quote(allocation) + " | ${pkgs.jq}/bin/jq -e '.ClientStatus == \"complete\"'", timeout=30)
        logs = nomad_server.succeed("nomad alloc logs -namespace telchar -stderr " + shlex.quote(allocation) + " build")
        for event in ["worker.started", "worker.phase.running", "worker.inputs.resolved", "worker.completed", "worker.inputs.summary", "worker.outputs.summary"]:
            assert event in logs, logs
        assert ("path=/nix/store/" in logs) == (level == "debug"), logs
        assert ("WORKER_BUILD_BEGIN" in logs) == (level == "debug"), logs
        assert ("WORKER_BUILD_END" in logs) == (level == "debug"), logs
        assert logs.index("worker.phase.running") < logs.index("worker.completed"), logs
        print("WORKER_TELEMETRY_LOGS " + level + " " + logs)
    for protocol, port in [("grpc", 4317), ("http/protobuf", 4318)]:
        command = "env RUST_LOG=info OTEL_EXPORTER_OTLP_PROTOCOL=" + shlex.quote(protocol) + " OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:" + str(port) + " TELCHAR_TRANSFER_ENDPOINT=private-marker://secret ${nomadWorker}/bin/telchar-nomad-worker >/tmp/failure.out 2>/tmp/failure.err"
        status, _ = nomad_client.execute(command, timeout=10)
        assert status == 1, status
        nomad_client.succeed("test ! -s /tmp/failure.out")
        errors = nomad_client.succeed("cat /tmp/failure.err")
        assert "worker.configuration.failed" in errors and "worker.failed" in errors, errors
        assert "private-marker" not in errors and "secret" not in errors, errors
    nomad_client.succeed("systemctl stop worker-collector")
    records = [json.loads(line) for line in nomad_client.succeed("cat /tmp/worker-telemetry.json").splitlines() if line.strip()]
    signals = {key: [resource for record in records for resource in record.get(key, [])] for key in ["resourceSpans", "resourceLogs", "resourceMetrics"]}
    for key, resources in signals.items():
        assert resources, key
        assert all(any(a["key"] == "service.name" and a["value"].get("stringValue") == "telchar-nomad-worker" for a in r["resource"]["attributes"]) for r in resources), key
    logs = [item for resource in signals["resourceLogs"] for scope in resource["scopeLogs"] for item in scope["logRecords"]]
    def attributes(item):
        return {a["key"]: a["value"] for a in item.get("attributes", [])}
    completed = [item for item in logs if attributes(item).get("event", {}).get("stringValue") == "worker.completed"]
    failures = [item for item in logs if attributes(item).get("event", {}).get("stringValue") == "worker.failed"]
    assert len(failures) == 2, failures
    assert len(completed) == 2, completed
    assert len({item["traceId"] for item in completed}) == 2, completed
    for item in completed + failures:
        assert any(span["traceId"] == item["traceId"] and span["name"] == "worker.execution" for resource in signals["resourceSpans"] for scope in resource["scopeSpans"] for span in scope["spans"]), item
    output_logs = [item for item in logs if attributes(item).get("event", {}).get("stringValue") == "worker.build.output"]
    assert output_logs and all(item["severityText"] == "DEBUG" for item in output_logs), output_logs
    assert len({item["traceId"] for item in output_logs}) == 1, output_logs
    metrics = [metric for resource in signals["resourceMetrics"] for scope in resource["scopeMetrics"] for metric in scope["metrics"]]
    assert any(metric["name"] == "telchar.worker.phase.duration" for metric in metrics), metrics
    print("WORKER_TELEMETRY_VERIFIED grpc http/protobuf info debug collector-unavailable success-failure-flush client-output-preserved")
  '';
}
