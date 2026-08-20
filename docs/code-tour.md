# Code tour

This guide maps Telchar's source tree to stable architectural boundaries. Read [Architecture](design.md) first for system contracts.

## Fast orientation

Telchar has three Rust crates:

- `crates/nix-worker-protocol` provides bounded, typed Nix worker-protocol I/O without Telchar policy.
- `crates/telchar` provides ingress, daemon composition, durable coordination, store access, and backend execution.
- `crates/telchar-nomad-worker` provides the allocation-side Nomad worker.

PostgreSQL migrations live in `crates/telchar/migrations/`. Nix packaging and executable system tests live under `nix/` and `tests/nixos/`.

## Follow a build

### 1. Ingress and daemon composition

A stock Nix client reaches the restricted `telchar serve-stdio` command configured by `nix/nixos-module.nix`. `crates/telchar/src/main.rs` performs CLI dispatch; `runtime.rs` owns frontend and daemon process composition. Identity normalization and the bounded local envelope live under `service::identity` and `service::ipc`.

Production environment configuration is captured at composition. In particular, `store/runtime.rs` parses the optional gateway-store endpoint once and constructs explicit query, import, export, closure, retention, and local-build dependencies. Lower-level environment constructors remain convenience APIs, not production orchestration.

Start with:

- `crates/telchar/src/main.rs`
- `crates/telchar/src/runtime.rs`
- `crates/telchar/src/service/identity.rs`
- `crates/telchar/src/service/ipc.rs`
- `crates/telchar/tests/openssh_ingress.rs`
- `crates/telchar/tests/ipc_frontend.rs`

### 2. Worker-protocol session

`service::session::SessionBuilder` assembles one authenticated session. `service/session/mod.rs` preserves the ordered transaction: negotiate, decode, validate, persist, retain, admit, execute or follow, validate outputs, release resources, and write a normal Nix response. `builder.rs` assembles dependencies; `input.rs` owns bounded input timing.

Worker-wire implementation is split across:

- `crates/nix-worker-protocol/src/lib.rs`: server-side decoding and intentional public re-exports;
- `protocol.rs`: versions, operations, limits, and allocation budgets;
- `requests.rs`: transfer and derivation request models;
- `stderr.rs`: structured worker stderr and activity frames;
- `client.rs`: typed daemon-client operations;
- `fixture.rs`: reusable protocol fixtures;
- `tests.rs` and `tests/*_contract.rs`: unit and exact byte contracts.

For operation behavior, see `crates/telchar/tests/operation_dispatch.rs` and its behavior modules under `tests/operation_dispatch/`.

### 3. Validation, configuration, and backend selection

The public crate API is grouped by domain rather than mirroring every source file:

- `telchar::backend`: backend contracts plus local, static SSH, and routing APIs;
- `telchar::build`: admitted build requests;
- `telchar::service`: configuration, ingress, sessions, limits, lifecycle services, and deployment policy;
- `telchar::store`: typed daemon access, transfer, validation, retention, and composition;
- `telchar::shared_build`: in-process coalescing, durable scheduling, and recovery;
- `telchar::nomad`: Nomad execution, authentication, callback, and transfer protocol;
- `telchar::fixture`: real-Nix and trace test infrastructure;
- `telchar::persistence`: durable domain operations.

`build/mod.rs` validates `BuildDerivation` shape, preserves fixed-output authority, and computes semantic identity. `service/config/` separates the public model, raw TOML, helpers, and validation. `service/metrics.rs` defines bounded-cardinality OTLP instruments used across scheduling, backends, cache, transfer, retention, and Nomad. `service/cache_publication.rs` owns the bounded post-success executable hook. `backend/routing.rs` selects a compatible operator-configured target and constructs its exact executor.

### 4. Gateway store

`store/daemon.rs` is the typed Nix daemon connection. Production store operations do not use the Nix C++ ABI.

Key modules:

- `store/query.rs`: exact path validity;
- `store/closure.rs`: bounded transitive input closure;
- `store/nar.rs`: NAR staging and validation;
- `store/promotion.rs`: metadata validation and typed import;
- `store/import.rs`, `store/export.rs`: transfer adapters;
- `store/retention.rs`: GC roots and output retention;
- `store/substitution.rs`: cache-only `EnsurePath` requests;
- `store/runtime.rs`: composition-time store dependencies;
- `service/disk_reserve.rs`, `service/transfer_limits.rs`: resource admission.

Backend success is insufficient. Every expected output must be transferred, validated, registered in the gateway store, and durably recorded.

### 5. Shared-build durability

`shared_build/mod.rs` coalesces equivalent requests inside one daemon. `shared_build/scheduler.rs` applies round-robin admission across quota subjects and FIFO order within a subject. Queue admission and backend capacity remain separate gates.

`persistence/` is split by durable aggregate:

- `migrations.rs`;
- `shared_builds.rs`;
- `build_requests.rs`;
- `executor.rs`;
- `attachments.rs`;
- `sessions.rs`;
- `leases.rs`;
- `callback_nonces.rs`.

`shared_build/recovery.rs` first trusts exact valid gateway outputs; otherwise it follows only the persisted backend name and execution identity.

Persistence integration authority is split into:

- `persistence_shared_builds.rs`;
- `persistence_sessions.rs`;
- `persistence_migrations.rs`;
- `persistence_leases.rs`.

### 6. Backend execution

- `backend/local.rs`: gateway-daemon or helper-driven local execution, bounded logs, cancellation, and output trust.
- `backend/static_ssh.rs`: configured SSH identity, transfer, execution, and exact-target recovery.
- `nomad/backend.rs`: deterministic jobs, submission, monitoring, adoption, and exact cancellation.

The local executor IPC service lives under `service/executor_service.rs`. Backend registration remains deliberately direct; no speculative plugin framework exists.

### 7. Nomad callback

Nomad security and transfer boundaries are separate:

- `nomad/callback_http.rs`: bounded WebSocket transport;
- `nomad/callback.rs`: exact execution resolution and replay admission;
- `nomad/authentication.rs`: workload JWT or scoped HMAC verification;
- `nomad/protocol.rs`: bounded TLNW frames and phases;
- `nomad/callback_service.rs`: listener lifecycle, input transfer, logs, outputs, receipts, and durable completion;
- `crates/telchar-nomad-worker/src/lib.rs`: allocation-side session.

See [Nomad](nomad.md) for deployment and protocol detail.

### 8. Ownership and maintenance

`service/singleton_ownership.rs` acquires and renews PostgreSQL ownership leases. Each takeover increments a fencing generation, and PostgreSQL triggers reject durable mutations from expired generations. `service/daemon_services.rs` owns cancellable maintenance and recovery threads. `runtime.rs` owns top-level composition; `runtime/daemon.rs` owns daemon socket, accepted-connection, and session lifecycle. Services start only after configuration, migration, reconciliation, and ownership succeed.

Never edit an applied migration. Add the next numbered migration and tests.

## Test and release authority

Integration tests are organized by behavior. Larger suites use a small root fixture module plus focused files:

- `operation_dispatch.rs` with `operation_dispatch/{protocol,store_transfer,build_lifecycle,scheduling,disconnect,validation}.rs`;
- the four `persistence_*.rs` suites listed above;
- `service_config.rs` with `service_config/{core,environment,nomad,static_ssh}.rs`;
- `nomad_backend.rs` with `nomad_backend/{client,execution,identity,rendering}.rs`;
- `ipc_frontend.rs` with `ipc_frontend/{handshake,ownership,readiness,sessions,socket}.rs`.

Shared PostgreSQL and admitted-request helpers live under `tests/support/`.

Release-relevant files:

- `flake.nix`: output composition;
- `nix/packages.nix`: packages and OCI archives;
- `nix/checks/rust.nix`: sandbox-compatible Rust checks;
- `nix/checks/policy.nix`: dependency and policy checks;
- `nix/checks/nixos.nix`: VM integration derivations;
- `nix/tests/oci-images.nix`: OCI metadata and loadability contract;
- `tests/nixos/lib.nix`: reusable VM topology.

Full process and PostgreSQL authority remains:

```text
nix develop -c cargo test --locked --workspace
```

Flake evaluation authority remains:

```text
NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build
```

## Finding common behavior

| Question | Start with |
| --- | --- |
| Why did a client request fail? | `service/session/`, then the matching worker-protocol operation |
| Why was a backend incompatible? | `build/request.rs`, `backend/mod.rs`, `backend/routing.rs` |
| Why is a build queued? | `shared_build/scheduler.rs`, shared-build persistence, scheduling tests |
| Why did duplicate requests execute once or twice? | `shared_build/mod.rs`, shared-build claims in `persistence/shared_builds.rs` |
| Why did restart mark work failed? | `shared_build/recovery.rs`, persisted backend and attempt fields |
| Why is an output rejected? | `store/export.rs`, `store/promotion.rs`, `store/nar.rs` |
| Why is a path retained? | `store/retention.rs`, `persistence/leases.rs` |
| Why did SSH ingress reject a client? | NixOS module, `service/identity.rs`, `service/ipc.rs` |
| Why did a Nomad callback fail? | callback HTTP → callback/authentication → callback service |
| Where is a configuration key defined? | `service/config/model.rs`, `raw.rs`, and `validation.rs` |
| Where is a database column used? | introducing migration, matching `persistence/` domain, integration suite |

## Safe change workflow

1. Identify the owning boundary.
2. Read the production symbol and closest integration test.
3. Add the narrowest failing authoritative test.
4. Make the smallest production change.
5. Run focused tests, diagnostics, workspace checks, and relevant Nix checks.
6. For protocol changes, update exact byte contracts and real-Nix fixtures.
7. For schema changes, add a migration plus failure and restart coverage.
8. For backend changes, preserve exact persisted identity and fail-closed recovery.
