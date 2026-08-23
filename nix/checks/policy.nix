# Defines the worker-protocol dependency-direction policy check.
{ pkgs }:
{
  release-publication-authority = pkgs.runCommand "telchar-release-publication-authority" { } ''
    root=${../..}
    prepare_workflow="$root/.github/workflows/prepare-release.yml"
    publish_workflow="$root/.github/workflows/release.yml"
    version_script="$root/scripts/prepare-release.py"
    publish_script="$root/scripts/publish-oci-images.sh"

    for file in "$prepare_workflow" "$publish_workflow" "$version_script" "$publish_script"; do
      if [ ! -f "$file" ]; then
        echo "release publication file is missing: $file" >&2
        exit 1
      fi
    done

    for value in \
      'workflow_dispatch:' \
      'pull-requests: write' \
      'scripts/prepare-release.py' \
      'gh pr create'
    do
      if ! grep -Fq -- "$value" "$prepare_workflow"; then
        echo "release preparation workflow omits: $value" >&2
        exit 1
      fi
    done

    for value in \
      'workflow_dispatch:' \
      'contents: write' \
      'packages: write' \
      'scripts/publish-oci-images.sh' \
      'gh release view' \
      'git ls-remote --exit-code --tags' \
      'gh release create' \
      '--draft' \
      'gh release edit' \
      '--draft=false'
    do
      if ! grep -Fq -- "$value" "$publish_workflow"; then
        echo "release publication workflow omits: $value" >&2
        exit 1
      fi
    done

    for value in \
      '^[1-9][0-9]{3}\.([1-9]|1[0-2])\.[0-9]+$' \
      'registry=ghcr.io/techcyte' \
      'telchar-oci:telchar' \
      'telchar-nomad-worker-oci:telchar-nomad-worker' \
      'telchar-nix-daemon-oci:telchar-nix-daemon' \
      'telchar-ssh-ingress-oci:telchar-ssh-ingress' \
      'docker://$registry/$image:$RELEASE_VERSION'
    do
      if ! grep -Fq -- "$value" "$publish_script"; then
        echo "release publication script omits: $value" >&2
        exit 1
      fi
    done

    if grep -Fq ':latest' "$publish_script" || grep -Fq 'tags:' "$publish_workflow"; then
      echo "release publication must remain manual and exact-version only" >&2
      exit 1
    fi

    touch "$out"
  '';

  supply-chain-authority = pkgs.runCommand "telchar-supply-chain-authority" { } ''
    root=${../..}
    deny_policy="$root/deny.toml"
    advisory_exceptions="$root/security/advisory-exceptions.toml"
    ci="$root/.github/workflows/ci.yml"
    release="$root/scripts/check-release.sh"

    for file in "$deny_policy" "$advisory_exceptions"; do
      if [ ! -f "$file" ]; then
        echo "supply-chain policy file is missing: $file" >&2
        exit 1
      fi
    done

    for command in \
      'cargo deny check advisories licenses sources' \
      'scripts/check-advisory-exceptions.py' \
      'scripts/check-oci-images.sh'
    do
      if ! grep -Fq "$command" "$ci" || ! grep -Fq "$command" "$release"; then
        echo "CI and release verification must execute supply-chain gate: $command" >&2
        exit 1
      fi
    done

    touch "$out"
  '';

  ignored-test-authority = pkgs.runCommand "telchar-ignored-test-authority" { } ''
    tests=${../..}/crates/telchar/tests
    private_reason='#[ignore = "private fixture paths are outside the production /nix/store namespace"]'
    helper_reason='#[ignore = "helper process for cross-PID-namespace authorization"]'

    assert_count() {
      file=$1
      expected=$2
      reason=$3
      actual=$(grep -Fxc "$reason" "$tests/$file" || true)
      if [ "$actual" -ne "$expected" ]; then
        echo "ignored-test policy changed for $file: expected $expected, found $actual" >&2
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
      echo "ignored-test policy changed: expected 8, found $ignored_count" >&2
      exit 1
    fi

    touch "$out"
  '';

  production-operation-authority = pkgs.runCommand "telchar-production-operation-authority" { } ''
    session=${../..}/crates/telchar/src/service/session/mod.rs
    protocol=${../..}/crates/nix-worker-protocol/src/protocol.rs

    for operation in \
      SetOptions \
      QueryValidPaths \
      QueryPathInfo \
      QueryMissing \
      AddMultipleToStore \
      NarFromPath \
      BuildDerivation \
      BuildPathsWithResults
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
      oci-images \
      nixos-oci-runtime \
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
