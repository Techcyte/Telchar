#!/bin/sh
set -eu

adr='docs/adr/telemetry-contract.md'
design='telchar-design.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'
protocol_manifest='crates/nix-worker-protocol/Cargo.toml'

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

[ -f "$adr" ] || {
	printf 'missing ADR: %s\n' "$adr" >&2
	exit 1
}

require "$adr" 'Application instrumentation uses `tracing` spans and events.'
require "$adr" 'structured logs, metrics, and distributed traces through OTLP over gRPC.'
require "$adr" 'Telchar service crate exclusively owns telemetry configuration'
require "$adr" '`nix-worker-protocol` may emit `tracing` spans and events, but it must not configure exporters or depend on OpenTelemetry SDK or exporter crates.'
require "$adr" 'Telemetry initializes before application events are emitted.'
require "$adr" 'Every application `tracing` event is exported through OTLP and written locally.'
require "$adr" 'Local log lines use `<time> trace_id=<trace_id> <level> <message>`.'
require "$adr" 'Non-error events write to standard output; error events write to standard error.'
require "$adr" 'Every signal includes configured service and resource attributes.'
require "$adr" 'request ID'
require "$adr" 'they do not replace OpenTelemetry trace or span IDs.'
require "$adr" 'Metric attributes use bounded, low-cardinality values.'
require "$adr" 'Sensitive fields are redacted consistently from logs, spans, and exporter errors.'
require "$adr" 'cannot crash Telchar, block startup indefinitely, recurse through telemetry errors, or grow queues without bound.'
require "$adr" 'real or protocol-compatible OTLP gRPC collector fixture'
require "$design" 'Observability is a bootstrap requirement, not a later operational feature.'
require "$design" 'The initial exporter supports OTLP over gRPC.'
require "$design" 'Exporter wiring remains in the Telchar service crate; `nix-worker-protocol` may depend on `tracing` for spans and events but must not configure exporters or depend on the OpenTelemetry SDK.'
require "$design" 'Request IDs remain domain identifiers and do not replace trace and span IDs.'
require "$design" 'Raw requester identities, credentials, store contents, source names, arbitrary derivation strings, and unbounded error text must not become metric labels.'
require "$plan" '- [x] T009A Define telemetry contract'

if grep -E '^(opentelemetry|opentelemetry_sdk|opentelemetry-otlp|tonic)[[:space:]]*=' "$protocol_manifest" >/dev/null; then
	printf 'protocol crate has an OpenTelemetry SDK or exporter dependency\n' >&2
	exit 1
fi

printf 'telemetry contract documentation check passed\n'
