# Defines the worker-protocol dependency-direction policy check.
{ pkgs }:
{
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
