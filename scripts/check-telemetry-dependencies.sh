#!/bin/sh
set -eu

protocol_manifest='crates/nix-worker-protocol/Cargo.toml'

if grep -E '^(opentelemetry|opentelemetry_sdk|opentelemetry-otlp|tonic)[[:space:]]*=' "$protocol_manifest" >/dev/null; then
	printf 'protocol crate has an OpenTelemetry SDK or exporter dependency\n' >&2
	exit 1
fi

printf 'telemetry dependency boundary check passed\n'
