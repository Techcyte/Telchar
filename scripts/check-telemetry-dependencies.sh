#!/bin/sh
set -eu

workspace_manifest='Cargo.toml'
service_manifest='crates/telchar/Cargo.toml'
protocol_manifest='crates/nix-worker-protocol/Cargo.toml'
lockfile='Cargo.lock'

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

require "$workspace_manifest" '[workspace.dependencies]'
require "$workspace_manifest" 'tracing = "0.1.44"'
require "$workspace_manifest" 'tracing-subscriber = { version = "0.3.23", default-features = false, features = ['
require "$workspace_manifest" 'tracing-opentelemetry = { version = "0.33.0", default-features = false, features = ['
require "$workspace_manifest" 'opentelemetry = { version = "0.32.0", default-features = false, features = ['
require "$workspace_manifest" 'opentelemetry_sdk = { version = "0.32.0", default-features = false, features = ['
require "$workspace_manifest" 'opentelemetry-otlp = { version = "0.32.0", default-features = false, features = ['
require "$workspace_manifest" 'opentelemetry-appender-tracing = "0.32.0"'
require "$workspace_manifest" 'tokio = { version = "1", features = ['
require "$workspace_manifest" 'opentelemetry-proto = { version = "0.32.0", default-features = false, features = ['
require "$workspace_manifest" 'tokio-stream = { version = "0.1", features = ["net"] }'
require "$workspace_manifest" 'tonic = { version = "0.14", features = ["transport"] }'

require "$service_manifest" 'tracing.workspace = true'
require "$service_manifest" 'tracing-subscriber.workspace = true'
require "$service_manifest" 'tracing-opentelemetry.workspace = true'
require "$service_manifest" 'opentelemetry.workspace = true'
require "$service_manifest" 'opentelemetry_sdk.workspace = true'
require "$service_manifest" 'opentelemetry-otlp.workspace = true'
require "$service_manifest" 'opentelemetry-appender-tracing.workspace = true'
require "$service_manifest" 'tokio.workspace = true'
require "$protocol_manifest" 'tracing.workspace = true'

if grep -E '^(opentelemetry|opentelemetry_sdk|opentelemetry-otlp|tonic)[[:space:]]*=' "$protocol_manifest" >/dev/null; then
	printf 'protocol crate has an OpenTelemetry SDK or exporter dependency\n' >&2
	exit 1
fi

for package in opentelemetry opentelemetry_sdk opentelemetry-otlp opentelemetry-appender-tracing tracing tracing-opentelemetry tracing-subscriber; do
	grep -F "name = \"$package\"" "$lockfile" >/dev/null || {
		printf 'missing resolved package in %s: %s\n' "$lockfile" "$package" >&2
		exit 1
	}
done

printf 'telemetry dependency boundary check passed\n'
