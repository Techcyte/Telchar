#!/bin/sh
set -eu

manifest='crates/nix-worker-protocol/Cargo.toml'

if grep -Ein '^(telchar|sqlx|postgres|russh|openssh|toml|serde|opentelemetry|opentelemetry_sdk|opentelemetry-otlp)[[:space:]]*=' "$manifest" >/dev/null; then
	printf 'protocol crate has a forbidden Telchar-domain dependency\n' >&2
	exit 1
fi

printf 'protocol dependency boundary check passed\n'
