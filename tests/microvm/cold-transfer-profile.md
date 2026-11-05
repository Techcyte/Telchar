# Bounded connected cold-transfer profile

Measurement/diagnosis only: no production optimization. Measured 2026-11-05 on
local x86_64 Linux, debug Rust builds, starting at `1e2c8602` (clean empty child
`0d176ae1`). Real production gateway session, Rust worker, TLS, Nix import/build,
ExecutionState, checkpoint/private daemon/overlay and durable reset all ran.
Only provider delivery/TLS termination are local simulated seams. Host page caches
were **not** flushed; these are fresh workers, not cold host caches.

## Commands and evidence

Recorded commands ran with `HERDR_ENV=1`, `CARGO_BUILD_JOBS=2`, as a nonroot user:

```sh
MICROVM_BENCH_SAMPLES=3 MICROVM_BENCH_INPUT_MIB=32 \
MICROVM_TRANSFER_PROFILE="$HOME/.local/state/microvm-cold-profile/worker.jsonl" \
  nix develop -c cargo test --locked -p telchar-nomad-worker \
  --test microvm_build microvm_reuse::cold_warm_connected_benchmark \
  -- --exact --ignored --nocapture --test-threads=1

# One separately bounded control, not an automatic retry:
MICROVM_BENCH_SAMPLES=2 MICROVM_BENCH_INPUT_MIB=32 \
MICROVM_BENCH_RELAY_NODELAY=1 \
MICROVM_TRANSFER_PROFILE="$HOME/.local/state/microvm-cold-profile/worker-nodelay.jsonl" \
  nix develop -c cargo test --locked -p telchar-nomad-worker \
  --test microvm_build microvm_reuse::cold_warm_connected_benchmark \
  -- --exact --ignored --nocapture --test-threads=1
```

Both commands passed, including all child fixtures; baseline had three cold jobs,
one excluded seed and three warm jobs, control had two cold, one seed and two warm.
No test failures, fixture skips or execution retries occurred in these runs.
Each cold/seed transferred 46,884,656 NAR payload bytes in seven paths; warm jobs
transferred zero input NAR bytes. All jobs returned/registered the distinct output
with the correct full-input SHA-256 and passed isolation/reset assertions.

External sanitized evidence directory: `$HOME/.local/state/microvm-cold-profile/`:

- `baseline.txt`: starting revision/time.
- `profile-baseline.log`, `profile-relay-nodelay.log`: complete command/test output.
- `worker.jsonl`, `worker-nodelay.jsonl`: per-path worker timing/counters, no secrets.
- `summary.json`: per-job sums joined in sequential fixture/path order.
- `instrumentation-final.patch`, `session.rs.instrumented`,
  `worker-lib.rs.instrumented`: exact temporary probes/source for independent review.
- `session.rs.original`, `worker-lib.rs.original`: pre-probe source; restored
  byte-for-byte before final validation. `MICROVM_TRANSFER_PROFILE` is **not** a
  retained production API; those two commands need the external probes to reproduce
  gateway/worker breakdowns. Relay counters/control remain in the benchmark.

## Boundaries

Temporary gateway probes surround each per-frame authority `verify`, metadata/frame
preparation, `TransferSession::accept`, and WebSocket `send` inside `InputSink`.
The per-path outer timer surrounds `GatewayStoreConnection::nar_from_path`.
Its residual is export reads plus loop/counter overhead, not pure export CPU time.
`accept` checks transfer metadata/order/length; it is **not** a payload hash pass.
Gateway export does not separately recompute a payload hash on this path. Worker
Nix import still validates the NAR/hash before successful registration.

Temporary worker probes surround each `add_to_store_nar` plus `source.finish`,
with accumulated WebSocket receive and frame decode/validation/copy time inside
`InputNarReader`. The residual includes Unix socket writes, Nix completion/validation
wait and overhead; it cannot isolate daemon hash CPU from disk or scheduling.
Worker import spans most of the same interval as gateway export/relay.

Retained relay probes time upstream read, decode, downstream TLS send/flush, and
count payloads/frames/maxima/stalls. No per-frame logs, packet capture, credentials,
input contents or unauthenticated/insecure transport were recorded.

**Do not add gateway + relay + worker times.** These are overlapping/pipelined
views of the same bytes. Within the gateway loop, instrumented operations are
serial; within the relay loop its operations are serial. Microsecond sums truncate
individual durations and include measurement overhead.

## Baseline results

Seconds, rounded; rows are measured cold samples in execution order. The seed is
excluded. Payload frame count was 5,727 / 5,727 / 5,728, maximum 8,192 bytes despite
the configured 256 KiB transfer upper bound.

| Boundary | Cold 1 | Cold 2 | Cold 3 |
| --- | ---: | ---: | ---: |
| Job through verified reset | 18.549 | 13.683 | 13.228 |
| Relay input phase | 17.056 | 12.472 | 12.099 |
| Gateway export outer wall | 16.949 | 12.414 | 12.076 |
| Gateway authority verification | 16.026 | 11.880 | 11.560 |
| Gateway frame preparation | 0.147 | 0.119 | 0.115 |
| Gateway transfer accept | 0.225 | 0.172 | 0.168 |
| Gateway send/flush | 0.476 | 0.189 | 0.182 |
| Gateway export residual | 0.074 | 0.053 | 0.051 |
| Relay upstream read/wait | 16.495 | 12.216 | 11.874 |
| Relay decode | 0.126 | 0.071 | 0.066 |
| Relay TLS send/flush | 0.415 | 0.172 | 0.145 |
| Worker import outer wall | 17.051 | 12.510 | 12.136 |
| Worker WebSocket receive | 16.610 | 12.348 | 11.971 |
| Worker decode/validate/copy | 0.226 | 0.110 | 0.112 |
| Worker import residual | 0.215 | 0.052 | 0.053 |
| Last relayed input through result | 0.299 | 0.295 | 0.289 |

Baseline cold median job/reset 13.683s; warm median 0.570s (observed maximum
0.620s). This is a tiny sequential sample, not tail-latency evidence. Cold input
transfer median 12.472s, maximum 17.056s. The first cold sample was slower in both
commands; neither run was randomized and host contention was uncontrolled.

## Diagnosis and bounded TCP control

**Strong evidence for this local connected fixture:** per-8-KiB authority checking
is the dominant cold input cost, occupying 94.6–95.7% of gateway export wall time
(about 2.0–2.8ms/frame). `Session::run_admitted` supplies a verifier that calls `load`:
`verify_available` performs ownership verification; `microvm_session_request` reads
and decodes/revalidates the active persisted request, which is compared with the
admitted request. These include PostgreSQL pool/connection validation and queries.
The measurement identifies the whole verifier, **not** a particular SQL query or
connection checkout as independently dominant. The source of the frame granularity
is `nix-worker-protocol::client::nar_from_path`'s 8,192-byte read buffer, forwarded
immediately by `InputSink`; a larger configured maximum does not coalesce it.

Worker import is primarily waiting for incoming frames (97.4–98.7% of import wall),
not demonstrated to be hash/disk throughput bound. Gateway send and relay TLS-send
wall times are small compared with verification. Baseline relay TLS-send maximum
was 16.914ms; no call reached 20ms. Thus receiver backpressure/TLS write stalls
are not the dominant measured synchronous cost. Socket buffering can hide receipt
and import lag; these figures do not prove the network is universally fast.

The fixture-only TCP_NODELAY control produced cold transfer 15.953 / 10.520s,
gateway verification 15.004 / 10.032s of 15.892 / 10.466s export wall (94.4–95.8%).
Relay TLS sends totaled 0.413 / 0.148s, maximum 7.281ms, no calls at least 20ms.
The verifier remains dominant. This is **not** a causal tuning speedup: unpaired
samples, run-order/host variation, and production peers' socket options unchanged.
Read waits of roughly 20–40ms still occur in both conditions and are not sufficient
to attribute them to Nagle/delayed ACKs. No TCP_INFO/packet-level attribution was
performed. Evidence rejects a dominant relay-write stall explanation here, not all
possible buffering effects on a real deployment.

## Final retained-code validation

After removing both production probes, the focused connected two-job correctness
test passed, as did `cargo clippy --locked -p telchar-nomad-worker --test microvm_build
-- -D warnings`. `cargo fmt --all -- --check` initially reported a single line-wrap
difference in the fixture's TCP_NODELAY declaration; it was corrected and the check
passed. Both the failing and corrected formatter outputs are retained externally.
No production execution was retried due to failure.

A final bounded benchmark with `MICROVM_BENCH_SAMPLES=1`,
`MICROVM_BENCH_INPUT_MIB=32`, `CARGO_BUILD_JOBS=2`, and neither temporary profiling
nor NODELAY environment variable passed after probe removal. Its cold input phase
was 11.173s, cold job/reset 12.454s, warm job/reset 0.566s. The cold seed (excluded)
was 10.477s input / 11.669s job/reset. The final source still emits relay profiles
but no gateway/worker probe output. See `final-no-production-probes.log`,
`final-connected.log`, `final-clippy.log`, `final-fmt.log`, and
`final-fmt-corrected.log`. All tee capture statuses were zero. A terminal wait
expired during compilation/file-lock waiting; observation continued without
restarting the command, which completed successfully.

The full workspace/VM/deployment/ARM/AWS gates were intentionally not run. LSP
extension diagnostics were unavailable to this worker's tool allowlist; focused
compiler/Clippy checks supplied type diagnostics instead.

## Next minimal experiment (not implemented)

If approved, first split the verifier timer into ownership availability, database
checkout/query, persisted-request decode/validation and comparison on the same
32 MiB connected path; capture calls and timings without changing fencing. This
isolates which repeated authority work dominates before selecting an optimization.
A subsequent candidate is bounded export-frame coalescing to reduce verifier calls
while retaining a verification immediately before every emitted frame, but that is
an unapproved behavioral/performance change requiring cancellation/ownership-loss
latency tests. Do not cache/bypass authority, hashes or store validation based on
these results. No production tuning was retained in this pass.
