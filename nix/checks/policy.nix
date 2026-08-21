# Defines the worker-protocol dependency-direction policy check.
{ pkgs }:
{
  ignored-test-authority =
    pkgs.runCommand "telchar-ignored-test-authority"
      {
        nativeBuildInputs = [ pkgs.jq ];
      }
      ''
        inventory=${../..}/docs/ignored-test-authority.json
        tests=${../..}/crates/telchar/tests
        private_reason='#[ignore = "private fixture paths are outside the production /nix/store namespace"]'
        helper_reason='#[ignore = "helper process for cross-PID-namespace authorization"]'

        assert_count() {
          file=$1
          expected=$2
          reason=$3
          actual=$(grep -Fxc "$reason" "$tests/$file" || true)
          if [ "$actual" -ne "$expected" ]; then
            echo "ignored-test inventory changed for $file: expected $expected, found $actual" >&2
            exit 1
          fi
        }

        assert_count store_export.rs 2 "$private_reason"
        assert_count output_transfer.rs 2 "$private_reason"
        assert_count operation_dispatch/store_transfer.rs 2 "$private_reason"
        assert_count store_promotion/real_store.rs 1 "$private_reason"
        assert_count ipc_auth.rs 1 "$helper_reason"

        ignored_count=$(grep -R -h '^#\[ignore' "$tests" | wc -l)
        if [ "$ignored_count" -ne 8 ]; then
          echo "ignored-test inventory changed: expected 8, found $ignored_count" >&2
          exit 1
        fi

        jq -e '
          .schema == 1 and
          (.ignoredTests | length == 8) and
          ([.ignoredTests[]] | all(
            (.test | type == "string" and length > 0) and
            (.reason | type == "string" and length > 0) and
            (.authority | length > 0)
          ))
        ' "$inventory" >/dev/null

        jq -r '.ignoredTests[].authority[]' "$inventory" | while read -r authority
        do
          file=$(printf '%s' "$authority" | cut -d: -f1)
          symbol=$(printf '%s' "$authority" | cut -d: -f3)
          if [ ! -f "${../..}/$file" ] || ! grep -Fq "$symbol" "${../..}/$file"; then
            echo "ignored-test replacement authority is missing: $authority" >&2
            exit 1
          fi
        done

        touch "$out"
      '';

  production-operation-authority =
    pkgs.runCommand "telchar-production-operation-authority"
      {
        nativeBuildInputs = [ pkgs.jq ];
      }
      ''
        inventory=${../..}/docs/supported-workload-operations.json
        session=${../..}/crates/telchar/src/service/session/mod.rs
        protocol=${../..}/crates/nix-worker-protocol/src/protocol.rs

        if [ ! -f "$inventory" ]; then
          echo "supported workload operation inventory is missing" >&2
          exit 1
        fi

        jq -e '
          .schema == 1 and
          (.workflows | length > 0) and
          ([.workflows[].operations[]] | length > 0) and
          ([.workflows[].operations[]] | all(type == "string" and length > 0)) and
          ([.workflows[]] | all(
            (.name | type == "string" and length > 0) and
            (.evidence | type == "string" and length > 0)
          ))
        ' "$inventory" >/dev/null

        jq -r '.workflows[].evidence' "$inventory" | while read -r evidence
        do
          evidence_path=$(printf '%s' "$evidence" | cut -d: -f1)
          evidence_path=$(printf '%s' "$evidence_path" | sed 's/ and .*//')
          if [ ! -f "${../..}/$evidence_path" ]; then
            echo "supported workload evidence is missing: $evidence_path" >&2
            exit 1
          fi
        done

        jq -r '.workflows[].operations[]' "$inventory" | sort -u | while read -r operation
        do
          if ! grep -Fq "Ok(WorkerOperation::$operation) =>" "$session"; then
            echo "supported workload operation lacks concrete production dispatch: $operation" >&2
            exit 1
          fi
        done

        if grep -Fq 'recognized-unimplemented' "$session" || grep -Fq 'is_fixture_allowed' "$protocol"; then
          echo "fixture observation still grants unsupported production-dispatch status" >&2
          exit 1
        fi

        touch "$out"
      '';

  release-workload-authority = pkgs.runCommand "telchar-release-workload-authority" { } ''
    release_script=${../..}/scripts/check-release.sh

    for check in \
      nixos-lix-local \
      nixos-fixed-output-local \
      nixos-oci-gateway \
      nixos-static-ssh-gateway \
      nixos-nomad-gateway
    do
      if ! grep -Fq ".#checks.x86_64-linux.$check" "$release_script"; then
        echo "release verification omits real-workload authority: $check" >&2
        exit 1
      fi
    done

    touch "$out"
  '';

  protocol-dependency-boundary = pkgs.runCommand "telchar-protocol-dependency-boundary" { } ''
    protocol_manifest=${../..}/crates/nix-worker-protocol/Cargo.toml
    workspace_manifest=${../..}/Cargo.toml

    if grep -Eq '(^|[[:space:]])(telchar|postgres|tokio|tonic|reqwest|tungstenite|opentelemetry|opentelemetry_sdk|opentelemetry-otlp|tracing-opentelemetry)[[:space:]]*=' "$protocol_manifest"; then
      echo "nix-worker-protocol contains a forbidden service dependency" >&2
      exit 1
    fi

    if ! grep -Eq '^tracing\.workspace[[:space:]]*=[[:space:]]*true$' "$protocol_manifest"; then
      echo "nix-worker-protocol must use workspace tracing" >&2
      exit 1
    fi

    if grep -Eq '(^|[[:space:]])(opentelemetry|opentelemetry_sdk|opentelemetry-otlp|tracing-opentelemetry)[[:space:]]*=' "$protocol_manifest"; then
      echo "nix-worker-protocol must not own telemetry exporters" >&2
      exit 1
    fi

    if ! grep -Eq '^tracing[[:space:]]*=' "$workspace_manifest"; then
      echo "workspace tracing dependency is missing" >&2
      exit 1
    fi

    touch "$out"
  '';
}
