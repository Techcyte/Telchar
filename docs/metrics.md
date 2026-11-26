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

Duration histograms with unit `s` use explicit boundaries from 10 microseconds
through two hours, including submillisecond and subsecond buckets. Values above
two hours fall in the overflow bucket. Percentiles remain bucket estimates;
individual traces provide timing detail. Byte/count histograms keep their own
SDK aggregation defaults.

Metric attributes must have bounded cardinality. Allowed dimensions describe configured or enumerated behavior, such as backend name and kind, operation, outcome, failure class, transfer direction, cache result, build mode, and fixed-output presence. Metrics must never contain requester identity, quota subject, request ID, trace ID, derivation or store path, shared-build key, execution ID, allocation ID, credential identity, endpoint, namespace, or arbitrary error text.

## Terminology

- **Session**: one authenticated frontend connection carrying a Nix worker-protocol conversation. A session can issue multiple operations serially.
- **Request**: one admitted `BuildDerivation` operation within a session.
- **Shared build**: one equivalence group for requests with the same semantic build identity.
- **Leader**: the request that owns durable execution for a shared build.
- **Follower**: an equivalent request waiting for its leader instead of executing again.
- **In flight**: a process-local shared build whose leader has not yet published a terminal result.
- **Queue admission**: permission for a durable leader to proceed under its quota subject's active-build limit.
- **Backend**: one independently named local, static SSH, or Nomad execution target.
- **Permit**: one configured unit of concurrent execution capacity on a backend. A backend with `maximum_concurrent_builds = 4` exposes four permits. Waiting for a permit means the build is admitted but backend execution capacity is saturated.
- **Selection**: choosing the exact compatible backend identity that is then persisted and used for permit acquisition and execution.
- **Transfer**: one bounded NAR movement between gateway store and execution environment.
- **Recovery monitor**: process-local observation of durable work adopted after startup.

## Service and build metrics

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.service.sessions` | gauge | `{session}` | Active daemon sessions. |
| `telchar.service.session.limit` | gauge | `{session}` | Configured daemon session capacity. |
| `telchar.service.session.rejections` | counter | `{session}` | Frontend sessions rejected by bounded reason, currently capacity. |
| `telchar.build.requests` | counter | `{request}` | Admitted build requests. |
| `telchar.build.request.duration` | histogram | `s` | Request service time from admission to terminal client result. |
| `telchar.build.executions` | counter | `{execution}` | Shared-build leader executions by terminal outcome. |
| `telchar.build.execution.duration` | histogram | `s` | Shared-build leader time from execution ownership to terminal result. |
| `telchar.build.output.count` | histogram | `{output}` | Expected output count per admitted build. |

`telchar.build.requests` and `telchar.build.output.count` carry the bounded `build_mode` and `fixed_output` attributes. Request and execution terminal instruments carry `outcome` and optional `failure_class`; they do not carry build-mode attributes. Request duration currently also records successful already-valid `BuildPathsWithResults` handling, so it is broader than pure backend execution latency.

## Shared-build scheduling

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.shared_build.leaders` | counter | `{build}` | Requests that own durable shared execution. |
| `telchar.shared_build.followers` | counter | `{build}` | Requests attached to existing shared work. |
| `telchar.shared_build.reused_results` | counter | `{build}` | Requests served from a durable terminal result. |
| `telchar.shared_build.in_flight` | gauge | `{build}` | Distinct equivalent-build groups currently coordinated in this daemon process. |
| `telchar.shared_build.waiting_followers` | gauge | `{request}` | Attached requests currently waiting for an in-process equivalent build to complete. |
| `telchar.shared_build.follower.wait.duration` | histogram | `s` | Follower wait time by bounded terminal outcome. |
| `telchar.shared_build.queue.depth` | gauge | `{build}` | Durable builds waiting for subject admission. |
| `telchar.shared_build.active` | gauge | `{build}` | Durable builds in running state. |
| `telchar.shared_build.collecting` | gauge | `{build}` | Durable builds collecting or validating outputs. |
| `telchar.shared_build.queue.wait.duration` | histogram | `s` | Time a leader waits for subject admission. |
| `telchar.shared_build.queue.admissions` | counter | `{build}` | Durable queue admissions. |
| `telchar.shared_build.live_log.truncations` | counter | `{truncation}` | Attached-session log queues that dropped one or more oldest chunks while accepting a callback chunk. |
| `telchar.shared_build.live_log.dropped_bytes` | counter | `By` | Log bytes dropped from or rejected by bounded attached-session queues. |

Queue depth and wait duration are primary subject-admission autoscaling and overload signals. `in_flight` and `waiting_followers` are authoritative process-local coalescing gauges. Live-log truncation counters expose slow attached clients without labeling their identity or build. Shared-build metrics have no requester, quota subject, derivation, path, request, or trace attributes.

## Backends

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.backend.permits.active` | gauge | `{permit}` | Active execution permits per configured backend. |
| `telchar.backend.permits.limit` | gauge | `{permit}` | Configured permit limit per backend. |
| `telchar.backend.permits.waiting` | gauge | `{request}` | Admitted executions currently waiting for a permit on each configured backend. |
| `telchar.backend.permit.wait.duration` | histogram | `s` | Time waiting for backend capacity. |
| `telchar.backend.selections` | counter | `{selection}` | Backend selections and selection failures. |
| `telchar.backend.executions` | counter | `{execution}` | Backend executions by outcome. |
| `telchar.backend.execution.duration` | histogram | `s` | Backend execution duration. |
| `telchar.static_ssh.hosts.available` | gauge | `{host}` | Configured static SSH hosts that completed the SSH and Nix worker-protocol readiness check. |
| `telchar.static_ssh.hosts.unavailable` | gauge | `{host}` | Configured static SSH hosts that did not complete the readiness check. |
| `telchar.static_ssh.health.checks` | counter | `{check}` | SSH and Nix readiness checks by ready or unavailable outcome. |
| `telchar.static_ssh.health.check.duration` | histogram | `s` | Complete readiness-check duration by outcome. |

Backend attributes are `backend.name`, `backend.kind`, and bounded `outcome` or `failure_class`. Configured backend names are operator-bounded. Permit utilization and wait duration are primary backend autoscaling signals. A selection failure with `failure_class=no_compatible_backend` distinguishes missing compatible capacity from saturation; `backend_unavailable` means compatible static SSH capacity exists in configuration but is not currently ready. Static SSH health gauges intentionally have no host labels and therefore retain bounded cardinality.

## Allocation worker

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.worker.phase.duration` | histogram | `s` | Completed worker phase duration, including failed phases. |

The `telchar-nomad-worker` service records `phase` (`configuration`, `manifest`,
`resolve-inputs`, `import-inputs`, `build`, `return-outputs`, `report-failure`) and
`outcome` (`success`, `failure`). Worker phases are local operation boundaries;
a successful build phase means its daemon call completed, not that the derivation
necessarily built successfully. No metric attributes identify individual builds.

## Configuration reload

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.configuration.reloads` | counter | `{reload}` | SIGHUP reload attempts by bounded `outcome` and optional `failure_class`. |
| `telchar.configuration.reload.duration` | histogram | `s` | Complete parse, validation, probe, assembly, and publication time. |
| `telchar.configuration.reload.static_ssh.added` | histogram | `{host}` | Static SSH hosts added by successful reload. |
| `telchar.configuration.reload.static_ssh.removed` | histogram | `{host}` | Static SSH hosts removed by successful reload. |

Reload metrics never contain configuration paths, backend names, destinations, credentials, or parser text.

## Cache and gateway store

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.cache.substitutions` | counter | `{attempt}` | Gateway substitution attempts by `hit` or `miss`; the current implementation classifies `EnsurePath` errors as misses. |
| `telchar.cache.substitution.duration` | histogram | `s` | Complete substitution attempt duration. |
| `telchar.post_build_hook.runs` | counter | `{attempt}` | Post-build hook attempts by outcome. |
| `telchar.post_build_hook.duration` | histogram | `s` | Post-build hook runtime. |
| `telchar.post_build_hook.pending` | gauge | `{hook}` | Durable pending hook delivery rows, including claimed work and delayed retries. |
| `telchar.post_build_hook.oldest_age` | gauge | `s` | Age of oldest pending hook delivery, zero when backlog is empty. |
| `telchar.post_build_hook.abandoned` | counter | `{hook}` | Hook delivery abandoned by bounded `failure_class`, including `retry-exhausted`, `missing-path`, `backlog-full`, and `hook-disabled`. |
| `telchar.store.validations` | counter | `{validation}` | Output validation attempts by outcome and authority kind. |
| `telchar.store.validation.duration` | histogram | `s` | Output validation duration. |

Cache policy, substituter names, URLs, keys, and credentials are never attributes.

### Operation attribution

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.store.operations` | counter | `{operation}` | Completed store boundary calls by `operation` and `outcome` (`success` or `failure`). |
| `telchar.store.operation.duration` | histogram | `s` | Wall time of each store boundary call, including failed calls. |
| `telchar.store.operations.active` | up/down sum | `{operation}` | Current calls by `operation`; balanced on unwinding. |
| `telchar.database.operations` | counter | `{operation}` | Database checkout attempts, validation attempts and query-method calls by outcome. |
| `telchar.database.operation.duration` | histogram | `s` | Database boundary duration by `operation` and outcome. |
| `telchar.database.lifecycle.duration` | histogram | `s` | Durable lifecycle function duration by bounded source-defined `operation`, including no-op maintenance calls. |
| `telchar.build.phase.duration` | histogram | `s` | Gateway phase duration by bounded `phase` and outcome. |
| `telchar.build.cached_output.phase.duration` | histogram | `s` | Cached-output lookup, validity, retention, integrity, request and lease work by phase/outcome. |

Store operations include `connect` (Unix socket connection), `handshake` (Nix
protocol negotiation), `is_valid_path`, `query_valid_paths`, `query_path_info`,
`query_missing`, `ensure_path`, `set_options`, `build_derivation`, and
`build_paths_with_results`. `nar_from_path` and `add_to_store_nar` include streaming
I/O and downstream/upstream backpressure. `stage_nar` includes reading, parsing,
hashing and writing the staging file; `promote_nar` is the promotion backend call;
`verify_registration` queries authoritative metadata. These boundaries can nest.
A successful daemon call does not imply a successful derivation result.

Database `checkout` measures each pool acquisition attempt, including waiting for
a new connection. Request-scoped acquisition may retry in bounded slices; sum
attempt durations to investigate contention, not just individual percentiles.
`validation` measures connection validation separately. Query methods are bounded
names (`query`, `query_one`, `query_opt`, `execute`, `batch_execute`, `simple_query`),
never SQL, query parameters or connection strings. Validation uses `simple_query`,
so those durations nest. Lifecycle timing includes pool and query costs and has
no outcome label: scope exit alone does not establish transaction success.

Store operation spans are available at `info`; database query spans require
`debug` to avoid default per-query trace volume. Metrics do not require verbose
logging. Gateway `server.phase` spans accompany phase duration metrics.

## Transfer and Nomad metrics

| Instrument | Kind | Unit | Meaning |
| --- | --- | --- | --- |
| `telchar.transfer.active` | gauge | `{transfer}` | Active NAR transfers by direction, purpose, and transport. |
| `telchar.transfer.objects` | counter | `{object}` | Completed transfer objects by direction, purpose, and transport. |
| `telchar.transfer.bytes` | counter | `By` | Completed transfer bytes by direction, purpose, and transport. |
| `telchar.transfer.object.size` | histogram | `By` | Completed object size. |
| `telchar.transfer.duration` | histogram | `s` | Successful and failed transfer duration. |
| `telchar.transfer.failures` | counter | `{failure}` | Terminal transfer failures by bounded failure class. |
| `telchar.transfer.rejections` | counter | `{rejection}` | Transfer admission or protocol rejections by bounded reason. |
| `telchar.recovery.attempts` | counter | `{attempt}` | Startup and monitor reconciliation attempts. |
| `telchar.recovery.duration` | histogram | `s` | Reconciliation attempt duration. |
| `telchar.recovery.outcomes` | counter | `{build}` | Reconciled builds by terminal or monitoring outcome. |
| `telchar.recovery.monitoring` | gauge | `{build}` | Adopted builds currently monitored after startup recovery. |
| `telchar.nomad.submissions` | counter | `{submission}` | Nomad submissions by outcome and backend. |
| `telchar.nomad.submission.duration` | histogram | `s` | Nomad submission request duration. |
| `telchar.nomad.pending` | gauge | `{allocation}` | Submitted jobs not yet observed complete or failed. |
| `telchar.nomad.placement.duration` | histogram | `s` | Time from submission until the first allocation is observed. |
| `telchar.nomad.executions` | counter | `{execution}` | Nomad job terminal outcomes. |
| `telchar.nomad.execution.duration` | histogram | `s` | Nomad job lifetime observed by Telchar. |
| `telchar.nomad.callback.connections` | gauge | `{connection}` | Active callback connections. |
| `telchar.nomad.callback.outcomes` | counter | `{connection}` | Callback connection terminal outcomes. |

Transfer attributes are bounded enums: `direction`, `purpose`, `transport`, and optional `failure_class`. Instrumented transports are gateway-store worker protocol, static SSH worker protocol, and Nomad callback transfer. Object bytes are recorded only when the exact NAR size is known and transfer succeeds.

Recovery attributes are bounded `operation`, `outcome`, and optional `failure_class`. Startup outcomes count durable builds reconciled as succeeded, failed, or still monitoring. Monitor demand is balanced across terminal completion, monitor failure, and daemon shutdown.

Nomad submission attributes are `backend.name`, `resource.profile`, bounded integer `nomad.priority`, and `outcome`. Pending and placement instruments carry `backend.name`; execution instruments carry `backend.name` and `outcome`.

Nomad pending demand, placement duration, backend permit utilization, and backend permit wait are intended for external autoscalers. Telchar exports demand and observed service behavior; it does not choose scaling policy.

`telchar.worker.input.validated_bytes` is a byte counter with only
`outcome=validated`. Shared worker input transfer records each requested NAR size
after Nix accepts the import and the transfer reader confirms completion.
Already-present inputs and failed or incomplete imports do not contribute.

`telchar.worker.output.accepted_bytes` is a byte counter with only
`outcome=accepted`. Shared worker output transfer records each NAR size only
after receiving a matching accepted gateway receipt. Rejected, incomplete, or
unacknowledged transfers do not contribute. Receipt acceptance is not itself the
durable terminal build result.

## Streaming I/O attribution

`telchar.transfer.io.duration` is a histogram in seconds;
`telchar.transfer.io.calls` and `telchar.transfer.io.bytes` are counters. All use
only `side` (`source` or `destination`) and `outcome` (`success` or `failure`). Each stream emits one
summary at scope exit, not one span or metric update per buffer. Bytes include
successfully processed partial data even if a later I/O call fails; these are
**not** the completed-object byte counters above. Outcome describes observed I/O
errors, not validated transfer or build success. Flush time and calls are included
in destination measurements.

Gateway/worker store import measures time inside its source reader; store export
measures time inside its destination writer. Static SSH staging uses the same
boundaries, and frontend NAR responses measure client writes. Source time may
include network receive, decoding or file reads. Destination time may include
network send, file writes or slow-consumer backpressure. Neither is pure network
latency or CPU time. Stream summaries overlap enclosing RPC and transfer duration;
do not add them to build wall time or subtract concurrent percentiles. Trace debug
summaries retain local correlation without labels identifying individual builds.

## Interpretation

Useful service-level views include:

- request rate, terminal outcome rate, and p50/p95/p99 request service time;
- queue depth and queue wait percentiles;
- backend permit utilization, permit wait, execution rate, failure rate, and execution latency by backend;
- leader-to-follower ratio and durable result reuse;
- cache hit ratio and substitution latency;
- transfer concurrency, throughput, object-size distribution, failure rate, and rejection rate;
- recovery throughput, failures, duration, and current monitoring demand;
- Nomad pending demand and placement latency;
- fixed-output versus input-addressed validation outcomes.

Counters and histograms are monotonic within a process lifetime. Gauges generally report current process state. Shared-build queue depth, running builds, and collecting builds are initialized from PostgreSQL after startup recovery and follow committed durable state transitions. Backend limits and session limits are established during composition; active session, permit, Nomad, and callback gauges change as the running process performs those operations.

## Stress-test workflow

Configure a reachable OTLP collector for gateway and allocation workers before
sending load. Nomad task rendering propagates gateway OTLP endpoint/protocol;
`127.0.0.1` therefore means each allocation's own network namespace, not the
gateway. Use operator-owned collectors/routes reachable from both, or configure
the worker launcher appropriately. Gateway metrics alone do not prove workers
successfully exported. Verify both `service.name=telchar` and
`service.name=telchar-nomad-worker` and inspect one connected trace first.

Start with `RUST_LOG=info`; enable database `debug` selectively for short diagnostic
windows, such as `RUST_LOG=info,telchar::persistence=debug`. For application trace
logging use `RUST_LOG=info,telchar=trace`, not global `trace`: HTTP/2 and OTLP
transport diagnostics can themselves flood the bounded export queues and displace
application events. Store RPC and worker operation spans are available at info. Export
queues are bounded and best-effort: unavailable or overloaded collectors can lose
telemetry. An incomplete trace is not evidence that a phase did not execute.
Monitor collector receive/export/drop counts and host CPU, memory, disk latency,
network bandwidth and PostgreSQL activity alongside Telchar. Telchar cannot
attribute kernel, compiler or remote Nix internals that it does not instrument.

Increase CI concurrency in steps and retain offered load, completed builds/sec,
error rate and request p50/p95/p99 for each interval. Separate unique builds from
coalescing/reuse workloads; distinguish cold inputs/outputs from warm stores.
Change one dimension at a time (concurrency, input size, output size, backend
capacity), allowing intervals long enough for long builds to complete. Compare
observations with identical telemetry settings; tracing itself has overhead.

| Symptom | Corroborating signal / next boundary |
| --- | --- |
| Queue wait grows, backend permits spare | Subject admission limits, fair queue demand; not automatically executor shortage. |
| Backend permit wait grows with full utilization | Execution capacity saturated; inspect backend build versus transfer phases. |
| Nomad pending/placement grows | Scheduler placement or allocation startup; worker build duration alone misses this. |
| Store RPC latency grows with connect/handshake counts | Connection churn or daemon pressure; compare metadata RPCs, staging, promotion and host disk activity. |
| Checkout duration grows | Pool demand or connection establishment; compare validation/query/lifecycle durations and PostgreSQL activity. |
| Transfer duration grows with source I/O time | Slow producer/read path, potentially network receive or source storage. |
| Transfer duration grows with destination I/O time | Slow consumer/write path, potentially network send, client backpressure or destination storage. |
| Staging grows while network throughput stays low | Parsing/hash/local staging I/O candidate; confirm host CPU/disk rather than calling it network time. |
| Backend execution stable, request latency grows | Input/load/admission, result validation/retention, durable completion or response path. |

Compute throughput from completed byte-counter rates and use object sizes and
concurrency as context. Stream partial-byte counters answer a different question;
never combine them with completed bytes. Do not subtract p99 values from different
histograms or sum nested phase durations to invent an unexplained residual.
For one build, inspect actual parent/child span intervals and concurrent overlap.
SSH spans measure gateway-visible boundaries only, not a distributed trace inside
the remote Nix daemon. Stock client work before Telchar ingress is outside this
trace and must be measured in CI if total job latency matters.

## Connected build traces

The gateway's `backend.execution` context parents the real Nomad
`worker.execution`. Each worker exports real child
phase spans (`worker.phase`, bounded `phase` and
`outcome` attributes) as well as the duration histograms above. Configuration
before the Nomad carrier is parsed remains a local startup trace. Nomad's
manifest phase parents the authenticated gateway `nomad.callback.execution`
round trip; gateway input export, build-and-collect and output collection have
separate spans. Do not infer cross-host ordering from wall-clock timestamps.

