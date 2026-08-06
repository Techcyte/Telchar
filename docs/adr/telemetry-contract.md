# Telemetry Contract

**Status:** Accepted

## Context

Telchar must make operational behavior observable from its first application path. The service needs structured local diagnosis and interoperable collection without allowing telemetry delivery failures to affect gateway correctness or availability.

## Decision

- Application instrumentation uses `tracing` spans and events. The OpenTelemetry metrics API supplies metric instruments; Telchar does not introduce another logging or metrics framework.
- Telchar exports structured logs, metrics, and distributed traces through OTLP over gRPC.
- The Telchar service crate exclusively owns telemetry configuration, subscriber installation, OpenTelemetry providers, OTLP exporters, batching, queue limits, export timeouts, flushing, and shutdown. `nix-worker-protocol` may emit `tracing` spans and events, but it must not configure exporters or depend on OpenTelemetry SDK or exporter crates.
- Telemetry initializes before application events are emitted. Shutdown flushes providers within configured bounds.
- OTLP endpoint, transport security, headers or credential references, enablement, batching, queue capacity, export timeout, and resource attributes are configuration.
- Every application `tracing` event is exported through OTLP and written locally. Local log lines use `<time> trace_id=<trace_id> <level> <message>`. They always write to standard error. Standard output belongs exclusively to the selected command's application protocol or machine-readable result; local telemetry must never write there. This policy applies to every command, not only `serve-stdio`, so future binary or structured stdout modes are safe by default.
- Every signal includes configured service and resource attributes. Long-lived and boundary-crossing work emits a span, event, metric, or the appropriate combination.
- Each request has a stable request ID. Request IDs are bounded domain correlation fields propagated through Telchar work; they do not replace OpenTelemetry trace or span IDs.
- Metric attributes use bounded, low-cardinality values. Raw requester identities, credentials, store contents, source names, arbitrary derivation strings, and unbounded error text must not become metric attributes. Sensitive fields are redacted consistently from logs, spans, and exporter errors.
- Exporter delivery failure cannot crash Telchar, block startup indefinitely, recurse through telemetry errors, or grow queues without bound. Failures are reported through a local non-telemetry path and recorded by bounded telemetry health counters where possible.
- Acceptance tests use a real or protocol-compatible OTLP gRPC collector fixture and prove correlated encoded log, metric, and trace traffic. Narrow unit tests may use in-memory exporters.

## Consequences

Every implementation task adds telemetry at its boundary rather than treating observability as follow-up work. The service crate gains OpenTelemetry dependencies and lifecycle responsibility; the protocol crate retains a lightweight `tracing`-only boundary.

Operators receive readable local diagnostics on standard error alongside remote export. Commands retain byte-transparent standard output for protocols and machine-readable results. Remote collector outages may lose bounded telemetry but cannot compromise request processing or shutdown.
