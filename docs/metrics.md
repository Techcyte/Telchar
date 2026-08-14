# OTLP metrics

Telchar exports metrics only through OTLP. Both OTLP/gRPC and OTLP/HTTP with protobuf encoding are supported. Telchar does not expose a Prometheus endpoint or implement a non-OTLP metrics transport.

Select transport with standard OpenTelemetry environment variables:

```bash
# Default
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4317

# OTLP/HTTP protobuf
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318
```

For OTLP/HTTP, Telchar appends the standard `/v1/traces`, `/v1/logs`, and `/v1/metrics` paths to the configured base endpoint. If no endpoint is configured, the transport defaults are `http://127.0.0.1:4317` for gRPC and `http://127.0.0.1:4318` for HTTP. Unsupported protocol values fail startup.

Metric attributes must have bounded cardinality. Allowed dimensions describe configured or enumerated behavior, such as backend name and kind, operation, outcome, failure class, transfer direction, cache result, build mode, and fixed-output presence. Metrics must never contain requester identity, quota subject, request ID, trace ID, derivation or store path, shared-build key, execution ID, allocation ID, credential identity, endpoint, namespace, or arbitrary error text.

## Service and build metrics

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.service.sessions` | gauge | `{session}` | Active daemon sessions. |
| `telchar.service.session.limit` | gauge | `{session}` | Configured daemon session capacity. |
| `telchar.build.requests` | counter | `{request}` | Admitted build requests. |
| `telchar.build.request.duration` | histogram | `s` | Request service time from admission to terminal client result. |
| `telchar.build.executions` | counter | `{execution}` | Shared-build leader executions by terminal outcome. |
| `telchar.build.execution.duration` | histogram | `s` | Shared-build leader time from execution ownership to terminal result. |
| `telchar.build.output.count` | histogram | `{output}` | Expected output count per admitted build. |

Build attributes are bounded enums: `build_mode` and `fixed_output`. Terminal instruments may add `outcome` and `failure_class`.

## Shared-build scheduling

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.shared_build.leaders` | counter | `{build}` | Requests that own durable shared execution. |
| `telchar.shared_build.followers` | counter | `{build}` | Requests attached to existing shared work. |
| `telchar.shared_build.reused_results` | counter | `{build}` | Requests served from a durable terminal result. |
| `telchar.shared_build.queue.depth` | gauge | `{build}` | Durable builds waiting for subject admission. |
| `telchar.shared_build.queue.wait.duration` | histogram | `s` | Time a leader waits for subject admission. |
| `telchar.shared_build.queue.admissions` | counter | `{build}` | Durable queue admissions. |

Queue depth and wait duration are primary subject-admission autoscaling and overload signals. Quota subjects are intentionally not metric attributes.

## Backends

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.backend.permits.active` | gauge | `{permit}` | Active execution permits per configured backend. |
| `telchar.backend.permits.limit` | gauge | `{permit}` | Configured permit limit per backend. |
| `telchar.backend.permit.wait.duration` | histogram | `s` | Time waiting for backend capacity. |
| `telchar.backend.selections` | counter | `{selection}` | Backend selections and selection failures. |
| `telchar.backend.executions` | counter | `{execution}` | Backend executions by outcome. |
| `telchar.backend.execution.duration` | histogram | `s` | Backend execution duration. |

Backend attributes are `backend.name`, `backend.kind`, and bounded `outcome` or `failure_class`. Configured backend names are operator-bounded. Permit utilization and wait duration are primary backend autoscaling signals. A selection failure with `failure_class=no_compatible_backend` distinguishes missing compatible capacity from saturation.

## Cache and gateway store

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.cache.substitutions` | counter | `{attempt}` | Gateway substitution attempts by hit, miss, or failure. |
| `telchar.cache.substitution.duration` | histogram | `s` | Complete substitution attempt duration. |
| `telchar.cache.publications` | counter | `{attempt}` | Publication hook attempts by outcome. |
| `telchar.cache.publication.duration` | histogram | `s` | Publication hook runtime. |
| `telchar.store.validations` | counter | `{validation}` | Output validation attempts by outcome and authority kind. |
| `telchar.store.validation.duration` | histogram | `s` | Output validation duration. |

Cache policy, substituter names, URLs, keys, and credentials are never attributes.

## Transfer and Nomad metrics

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.transfer.objects` | counter | `{object}` | Completed transfer objects by direction and purpose. |
| `telchar.transfer.bytes` | counter | `By` | Completed transfer bytes by direction and purpose. |
| `telchar.transfer.object.size` | histogram | `By` | Completed object size. |
| `telchar.transfer.duration` | histogram | `s` | Transfer operation duration. |
| `telchar.transfer.rejections` | counter | `{rejection}` | Transfer admission or protocol rejections by bounded reason. |
| `telchar.nomad.submissions` | counter | `{submission}` | Nomad submissions by outcome and backend. |
| `telchar.nomad.submission.duration` | histogram | `s` | Nomad submission request duration. |
| `telchar.nomad.pending` | gauge | `{allocation}` | Submitted jobs not yet observed complete or failed. |
| `telchar.nomad.placement.duration` | histogram | `s` | Time from submission until the first allocation is observed. |
| `telchar.nomad.executions` | counter | `{execution}` | Nomad job terminal outcomes. |
| `telchar.nomad.execution.duration` | histogram | `s` | Nomad job lifetime observed by Telchar. |
| `telchar.nomad.callback.connections` | gauge | `{connection}` | Active callback connections. |
| `telchar.nomad.callback.outcomes` | counter | `{connection}` | Callback connection terminal outcomes. |

Nomad pending demand, placement duration, backend permit utilization, and backend permit wait are intended for external autoscalers. Telchar exports demand and observed service behavior; it does not choose scaling policy.

## Interpretation

Useful service-level views include:

- request rate, terminal outcome rate, and p50/p95/p99 request service time;
- queue depth and queue wait percentiles;
- backend permit utilization, permit wait, execution rate, failure rate, and execution latency by backend;
- leader-to-follower ratio and durable result reuse;
- cache hit ratio and substitution latency;
- transfer throughput, object-size distribution, and rejection rate;
- Nomad pending demand and placement latency;
- fixed-output versus input-addressed validation outcomes.

Counters and histograms are monotonic within a process lifetime. Gauges report current process-observed state. Backend limits and session limits are established during composition; active session, queue, permit, Nomad, and callback gauges change as the running process performs those operations.
