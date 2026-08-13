# Code tour

This guide maps Telchar's source tree to the behavior an operator or contributor sees. Read [Architecture](design.md) first for the system contract; use this document to find its implementation.

## Fast orientation

Telchar has three Rust crates:

- `crates/nix-worker-protocol` implements bounded, typed Nix worker-protocol I/O. It deliberately knows nothing about Telchar identity, scheduling, PostgreSQL, or backends.
- `crates/telchar` implements the gateway daemon, restricted frontend, durable coordinator, gateway-store boundary, and backends.
- `crates/telchar-nomad-worker` is the small allocation-side process used only by the Nomad backend.

Nix packaging and VM tests live under `nix/` and `tests/nixos/`. PostgreSQL schema changes live in `crates/telchar/migrations/`. Rust integration tests usually mirror the production module they exercise.

## Follow a build through the system

### 1. Client ingress

A stock Nix client connects to OpenSSH. The NixOS module in `nix/nixos-module.nix` configures the restricted user and forced command. The forced command runs:

```text
telchar serve-stdio
```

`crates/telchar/src/main.rs` implements that command. It reads authenticated identity supplied by the operator-controlled ingress, normalizes it through `identity.rs`, creates a bounded `IpcEnvelope` from `ipc.rs`, and relays the worker-protocol byte stream to the daemon's Unix socket.

Start here when debugging SSH connection failures, forced-command behavior, daemon socket setup, peer UID checks, or process shutdown:

- `nix/nixos-module.nix`
- `crates/telchar/src/main.rs`
- `crates/telchar/src/identity.rs`
- `crates/telchar/src/ipc.rs`
- `crates/telchar/tests/openssh_ingress.rs`
- `crates/telchar/tests/ipc_frontend.rs`

### 2. Worker-protocol session

The daemon accepts the authenticated IPC envelope and calls `session::run_worker_session`. `crates/telchar/src/session.rs` is the main request orchestrator. It negotiates the protocol, dispatches supported operations, manages leases and attachments, admits builds, waits for shared results, and writes ordinary Nix responses.

Wire parsing and encoding come from `crates/nix-worker-protocol/src/lib.rs`. That large module owns protocol versions, bounded readers and writers, operation types, daemon-client operations, activity frames, and `BuildResult` encoding. Contract tests in `crates/nix-worker-protocol/tests/` pin exact byte behavior.

Start here for an unsupported operation, framing error, protocol-version disagreement, or incorrect client-visible result:

- `crates/telchar/src/session.rs`
- `crates/nix-worker-protocol/src/lib.rs`
- the matching file under `crates/nix-worker-protocol/tests/`
- `crates/telchar/tests/operation_dispatch.rs`
- `crates/telchar/tests/stdio_handshake.rs`

### 3. Build validation and identity

`crates/telchar/src/build_request.rs` converts a decoded `BuildDerivation` into Telchar's bounded admitted request. It validates build mode, system, required features, outputs, and request shapes, then computes the semantic digest used to reject inconsistent requests for the same derivation.

`config.rs` loads strict TOML and constructs the operator-owned backend fleet and limits. `backend.rs` defines backend capabilities, target compatibility, permits, execution requests, logs, and terminal results. `backend_routing.rs` binds configured targets to concrete executors.

Start here for admission rejection, wrong backend selection, feature mismatch, or configuration parsing:

- `crates/telchar/src/build_request.rs`
- `crates/telchar/src/config.rs`
- `crates/telchar/src/backend.rs`
- `crates/telchar/src/backend_routing.rs`
- `crates/telchar/tests/build_request.rs`
- `crates/telchar/tests/build_backend.rs`
- `crates/telchar/tests/service_config.rs`

### 4. Input closure and gateway store

The gateway store is reached through the typed Nix daemon connection in `store_daemon.rs`. Production code does not use the Nix C++ ABI or shell commands for store operations.

Input handling crosses these modules:

- `store_query.rs` answers exact validity queries.
- `store_closure.rs` walks references to compute the admitted transitive closure.
- `nar.rs` validates and stages an incoming NAR.
- `store_promotion.rs` validates declared metadata, imports with `AddToStoreNar`, and confirms registration.
- `store_import.rs` provides the typed import adapter.
- `store_export.rs` streams registered NARs and verifies their metadata.
- `store_retention.rs` owns durable GC roots and output retention.
- `disk_reserve.rs` rejects work before reserved storage would be consumed.
- `transfer_limits.rs` enforces counts, bytes, rates, and time bounds.

A useful correctness rule: a successful backend result is not enough. Every exact declared output must pass the export/import and gateway-store confirmation path before durable success.

### 5. Shared-build coordination

Equivalent builds meet in two layers:

- `shared_build.rs` coalesces requests within the running daemon and gives callers leader or follower roles.
- `persistence.rs` stores the durable shared-build row, queue position, attempt, admitted specification, backend identity, and terminal result.

`shared_build_scheduler.rs` applies round-robin admission across quota subjects and FIFO order within each subject. Queue admission and backend permits are separate. The first requester remains quota owner through terminal completion; followers do not consume another execution allocation or backend permit.

`shared_build_recovery.rs` reconciles nonterminal rows after restart. Exact valid gateway outputs win first. Otherwise recovery follows only the persisted backend and execution identity.

Start here for duplicate builds, stuck queues, quota accounting, follower behavior, or recovery:

- `crates/telchar/src/shared_build.rs`
- `crates/telchar/src/shared_build_scheduler.rs`
- `crates/telchar/src/shared_build_recovery.rs`
- `crates/telchar/src/persistence.rs`
- `crates/telchar/migrations/0009_shared_builds.sql` through `0015_shared_build_specification.sql`
- matching `shared_build_*` and `persistence.rs` integration tests

### 6. Backend execution

#### Local

`local_executor.rs` invokes normal-mode `BuildDerivation` against the configured gateway-side Nix daemon. It bounds logs and diagnostics, owns child cancellation and reaping, checks the exact output set, and reports output trust. `executor_service.rs` is the bounded idempotent IPC service used by the local executor path.

#### Static SSH

`static_ssh_backend.rs` uses only the configured SSH destination, identity, host-key file, and helper command. It copies admitted paths to the remote store, executes, streams logs, imports every expected output, and recovers only through that exact configured backend.

#### Nomad

`nomad_backend.rs` renders deterministic jobs, submits them, monitors exact allocations, adopts persisted executions, and purges exact jobs on cancellation or timeout. Data does not travel through the Nomad API. The allocation connects to Telchar's callback service.

See the [Nomad guide](nomad.md) for deployment details.

### 7. Nomad callback and transfer

The callback path is split so each security boundary is visible:

- `nomad_callback_http.rs` handles bounded WebSocket upgrade, exact subprotocol, binary messages, and keepalive.
- `nomad_callback.rs` resolves authentication to one exact active durable execution and reserves replay authority.
- `nomad_transfer_authentication.rs` verifies workload-identity JWTs or scoped HMAC capabilities.
- `nomad_transfer_protocol.rs` defines every bounded TLNW frame and phase type.
- `nomad_callback_service.rs` owns listener lifetime and drives input requests, build start, logs, output receipts, and durable completion.
- `crates/telchar-nomad-worker/src/lib.rs` implements the allocation side of the same session.

When debugging Nomad, separate three classes first:

1. job submission or allocation identity: `nomad_backend.rs`;
2. WebSocket or authentication: callback HTTP, callback admission, and authentication modules;
3. input, build, log, or output phase: transfer protocol, callback service, and allocation worker.

### 8. Durability, ownership, and maintenance

`persistence.rs` is the database authority. It is intentionally large because transactions encode cross-table lifecycle invariants. Search it by the public operation name used by the caller rather than reading from top to bottom.

`singleton_ownership.rs` holds the dedicated PostgreSQL advisory-lock connection. Lock loss fences the process permanently. `daemon_services.rs` owns cancellable maintenance and recovery threads. `main.rs` starts these services only after configuration, migration, store reconciliation, and singleton ownership succeed.

The migration ledger is ordered. Never edit an applied migration; add the next numbered file and corresponding persistence tests.

## Source tree reference

### `crates/nix-worker-protocol`

- `src/lib.rs`: complete reusable worker-wire implementation.
- `tests/*_contract.rs`: exact operation and daemon-client byte contracts.
- `fuzz/fuzz_targets/primitive_framing.rs`: hostile primitive-framing fuzz target.

This crate must stay free of Telchar policy and infrastructure dependencies. `scripts/check-protocol-boundary.sh` enforces that rule.

### `crates/telchar/src`

- `main.rs`: binary entry points and daemon lifecycle.
- `lib.rs`: public module surface used by integration tests.
- `session.rs`: one client session and supported operation dispatch.
- `config.rs`: strict service configuration.
- `persistence.rs`: migrations and durable lifecycle transactions.
- `backend.rs`, `backend_routing.rs`: backend-neutral contract and configured dispatch.
- `local_executor.rs`, `static_ssh_backend.rs`, `nomad_backend.rs`: concrete backend implementations.
- `nomad_callback*.rs`, `nomad_transfer*.rs`: allocation callback security and data protocol.
- `store_*.rs`, `nar.rs`: gateway-store queries, closure, transfer, validation, import, export, and retention.
- `shared_build*.rs`: coalescing, fair scheduling, and exact recovery.
- `identity.rs`, `ipc.rs`: authenticated frontend boundary.
- `telemetry.rs`: structured local and OTLP signals.
- `transfer_limits.rs`, `disk_reserve.rs`: resource bounds.
- `nix_fixture.rs`, `worker_trace.rs`: real-Nix test infrastructure and compatibility traces.

### `crates/telchar/tests`

Integration tests are organized by production concern. Most files directly match a module name. Large suites worth knowing:

- `operation_dispatch.rs`: supported stock-Nix operations, limits, timeouts, and cleanup.
- `ipc_frontend.rs`: real frontend/daemon process behavior and ownership fencing.
- `persistence.rs`: schema and transactional lifecycle authority.
- `service_config.rs`: complete strict configuration coverage.
- `nomad_backend.rs`: job rendering, API behavior, exact adoption, timeout, and cancellation.
- `nix_fixture.rs`: real private Nix daemon, store, build, import, export, and GC behavior.

`tests/support/postgres.rs` provisions isolated test databases. `tests/support/build_request.rs` creates admitted requests without duplicating parsing setup.

### `crates/telchar-nomad-worker`

- `src/lib.rs`: allocation session implementation.
- `src/main.rs`: environment-driven executable entry point.
- `tests/worker.rs`: end-to-end worker behavior against bounded fake sockets and Nix endpoints.

### PostgreSQL migrations

- `0001`–`0008`: protocol sessions, request lifecycle, leases, local execution, credentials, and retained-size accounting.
- `0009`–`0013`: shared-build authority, fair scheduling, and backend attempts.
- `0014`: Nomad callback replay protection.
- `0015`: exact durable admitted build specification.

### Nix and release files

- `flake.nix`: thin output composition only.
- `nix/packages.nix`: Rust packages and OCI image archives.
- `nix/nixos-module.nix`: production NixOS service module.
- `nix/checks/rust.nix`: sandbox-compatible Rust checks.
- `nix/checks/policy.nix`: dependency and documentation policy checks.
- `nix/checks/nixos.nix`: VM integration derivations.
- `nix/tests/oci-images.nix`: OCI output contract.
- `tests/nixos/lib.nix`: reusable VM topology constructors.
- `scripts/check-release.sh`: authoritative selected release suite.

## Finding common behavior

| Question | Start with |
| --- | --- |
| Why did a client request fail? | `session.rs`, then the matching protocol operation in `nix-worker-protocol` |
| Why was a backend incompatible? | `build_request.rs`, `backend.rs`, `backend_routing.rs` |
| Why is a build queued? | `shared_build_scheduler.rs`, persistence queue operations, scheduling tests |
| Why did duplicate requests execute once or twice? | `shared_build.rs`, shared-build claims in `persistence.rs` |
| Why did restart mark work failed? | `shared_build_recovery.rs`, persisted attempt/backend fields |
| Why is an output rejected? | `store_export.rs`, `store_promotion.rs`, `nar.rs` |
| Why is a store path still retained? | `store_retention.rs`, `store_leases` persistence operations |
| Why did SSH ingress reject a client? | NixOS module, `identity.rs`, `ipc.rs`, `openssh_ingress.rs` |
| Why did a Nomad callback fail? | callback HTTP → callback admission/authentication → callback service |
| Why was a Nomad job not adopted? | `nomad_backend.rs`, `shared_build_recovery.rs` |
| Where is a configuration key defined? | public structs and raw structs in `config.rs`; examples in `service_config.rs` |
| Where is a database column used? | migration introducing it, then search `persistence.rs` and its tests |
| Which test is release-authoritative? | `scripts/check-release.sh`, then `nix/checks/nixos.nix` |

## Safe change workflow

1. Identify the owning boundary from this tour.
2. Read the complete production symbol and its closest integration tests.
3. Add a failing test at the narrowest authoritative level.
4. Change the smallest production surface.
5. Run the focused test, LSP diagnostics, workspace checks, and relevant Nix check.
6. For protocol changes, update exact byte-contract tests and real-Nix fixtures.
7. For schema changes, add a migration and persistence failure/restart coverage.
8. For backend changes, preserve exact persisted identity and fail-closed recovery.
9. For NixOS or Nomad changes, run the matching VM derivation from `nix/checks/nixos.nix`.

Every maintained Rust, Nix, SQL, and shell source file begins with a one-line purpose comment. `scripts/check-source-file-guides.sh` and the `source-file-guides` flake check keep that map from silently decaying.
