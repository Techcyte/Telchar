#!/bin/sh
set -eu

policy='docs/adr/nix-worker-protocol-boundary.md'
manifest='crates/nix-worker-protocol/Cargo.toml'

require() {
	text=$1
	if ! grep -F -- "$text" "$policy" >/dev/null; then
		printf 'missing required protocol-boundary text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$policy" ] || {
	printf 'missing protocol boundary policy: %s\n' "$policy" >&2
	exit 1
}

for responsibility in \
	'wire primitives' \
	'negotiation' \
	'operations' \
	'messages' \
	'activity/error frames' \
	'result types' \
	'compatibility fixtures' \
	'property tests' \
	'fuzz targets'; do
	require "$responsibility"
done

for forbidden in \
	'identity' \
	'scheduler' \
	'PostgreSQL' \
	'SSH ingress' \
	'backend' \
	'cache' \
	'service configuration'; do
	require "$forbidden"
done

if grep -Ein '^(telchar|sqlx|postgres|russh|openssh|toml|serde|opentelemetry|opentelemetry_sdk|opentelemetry-otlp)[[:space:]]*=' "$manifest" >/dev/null; then
	printf 'protocol crate has a forbidden Telchar-domain dependency\n' >&2
	exit 1
fi

printf 'protocol dependency boundary check passed\n'
