# Telchar: agent orientation

## Start here

Telchar is a self-hosted Nix build gateway, not a CI scheduler. Stock Nix clients use `ssh-ng`; Telchar validates and coalesces requests, queues them fairly, executes on local Nix, static SSH, or Nomad, and returns validated outputs through the gateway store.

This file is a navigation map and change checklist. Read the owning implementation and its closest tests before changing behavior; do not rediscover the entire repository for every question.

- [README](README.md): product scope, deployment entry points, development commands.
- [Architecture](docs/design.md): lifecycle, trust boundaries, durability, non-goals.
- [Code tour](docs/code-tour.md): detailed module map and question-to-source index.
- [Compatibility](docs/compatibility.md): supported client/backend combinations and executable evidence.
- [Operations](docs/operations.md), [Nomad](docs/nomad.md), [IPC/transfer messages](docs/ipc-and-transfer-messages.md), [metrics](docs/metrics.md): consult for the relevant boundary.
- [Roadmap](docs/roadmap.md): proposals, not implemented functionality.

For questions and reviews, stay read-only unless Travis requests changes. For implementation, use Jujutsu (`jj`), inspect `jj status` first, and ask how to handle unrelated existing changes. Preserve work belonging to other tasks. Keep changes small and make logical, described changesets. Discuss architectural changes before implementing them.

## Repository map

Four Rust 2024 crates form the Cargo workspace. Nix flake outputs target `x86_64-linux` only; `flake.lock` pins the tool and system-test dependencies.

| Location | Responsibility |
| --- | --- |
| `crates/nix-worker-protocol/` | Bounded Nix wire primitives, negotiation, typed requests/results, daemon client, stderr/activity frames, fixtures, property tests, fuzz targets. No Telchar policy. |
| `crates/telchar/` | Gateway library and `telchar` binary: ingress, coordination, stores, backends, operator inspection, telemetry. |
| `crates/telchar-nomad-worker/` | Allocation-side worker binary/library: callback authentication, inputs, Nix build, logs, output return. Depends on the gateway, protocol, and telemetry crates. |
| `crates/telchar-telemetry/` | Shared local tracing, bounded OTLP exporters, and phase progress reporting for the gateway and allocation worker. No scheduling or persistence policy. |
| `crates/telchar/migrations/` | Numbered PostgreSQL schema migrations, embedded by `persistence/migrations.rs`. |
| `nix/` | Packages, OCI archives, Telchar NixOS service module, SSH identity adapter, product flake checks. |
| `nix/checks/nixos/`, `tests/nixos/` | Executable VM contracts and shared fixtures. `tests/nixos/lib.nix` exports constructors; `common.nix`, `ingress.nix`, `static-ssh.nix`, `nomad.nix`, and `recovery.nix` own their topologies and setup. |
| `deploy/` | Restricted SSH ingress and Nix-daemon container entrypoints. |
| `examples/` | Operator-owned NixOS compositions and a commented Nomad deployment example. |
| `scripts/`, `.github/workflows/`, `security/`, `deny.toml` | Fixtures, release tooling, CI, dependency/image policy and advisory exceptions. |

### Follow a build

```text
stock client -> OpenSSH forced command -> telchar serve-stdio
             -> authenticated Unix IPC -> telchar daemon
             -> validate / retain inputs / claim shared build / queue
             -> gateway substitution or selected local / SSH / Nomad backend
             -> validate and register every gateway output
             -> durable terminal result -> normal Nix BuildResult
```

The frontend has no database or scheduling authority. The daemon owns coordination and lifecycle. The runtime uses blocking connections and bounded service/session threads; Tokio dependencies do not make the whole service an async application.

Rust paths below are relative to `crates/telchar/src/`; `deploy/` is repository-root-relative:

| Task | Start with |
| --- | --- |
| CLI dispatch and startup | `main.rs`, `runtime.rs`; daemon socket/session lifecycle in `runtime/daemon.rs` |
| Identity and local frontend authentication | `service/identity.rs`, `service/ipc.rs`; also `deploy/ssh/` |
| Worker operation handling and ordered build transaction | `service/session/mod.rs`; dependencies in `builder.rs`, bounded input timing in `input.rs` |
| Admitted build identity, fixed-output authority | `build/mod.rs`, `build/derivation.rs` |
| Configuration keys and validation | `service/config/{model,raw,validation,helpers}.rs`, `service/config/mod.rs` |
| Inventory/token reload and SSH discovery | `service/config_reload.rs`, `service/static_ssh_consul.rs`, `backend/static_ssh/health.rs` |
| Compatibility selection and backend permits | `backend/mod.rs`, `backend/routing.rs` |
| Local or SSH execution | `backend/local.rs`, `backend/static_ssh.rs` |
| Coalescing, fair queueing, restart recovery | `shared_build/{mod,scheduler,recovery}.rs` |
| Durable aggregates | `persistence/{shared_builds,build_requests,executor,attachments,sessions,leases,callback_nonces}.rs`; connections in `connection.rs` |
| Ownership fencing and maintenance | `service/singleton_ownership.rs`, `service/daemon_services.rs`; [ownership ADR](docs/adr/singleton-ownership-lease.md) |
| Typed gateway daemon I/O and dependency composition | `store/daemon.rs`, `store/runtime.rs` |
| Path queries, closures, imports/exports, NAR validation | `store/{query,closure,import,export,nar,promotion}.rs` |
| GC roots, substitution, transfer and disk admission | `store/{retention,substitution}.rs`, `service/{transfer_limits,disk_reserve}.rs` |
| Nomad job rendering/submission/adoption | `nomad/backend.rs` |
| Nomad callback security and transfer | `nomad/{callback_http,callback,authentication,protocol,callback_service}.rs` |
| Read-only operator commands | `operator.rs`, `operator/report.rs` |
| Tracing/OTLP composition and metric instruments | `crates/telchar-telemetry/src/` (repository-root-relative), `service/metrics.rs`, `service/activity.rs`; gateway exporter tests in `../tests/telemetry.rs`, cache publication in `service/cache_publication.rs` |

`telchar executor` is a separate IPC service composed in `runtime.rs`, with `service/executor_service.rs` and durable `persistence/executor.rs` records. Do not confuse it with the daemon's directly constructed local backend.

The public library groups APIs into `backend`, `build`, `fixture`, `nomad`, `persistence`, `service`, `shared_build`, and `store`. Follow those domain boundaries instead of adding arbitrary root re-exports or a plugin framework.

## Invariants to preserve

- **One trusted store domain.** Authenticated clients and configured executors are trusted organizational participants; build payloads still require Nix sandboxing. Store-path knowledge is not tenant authorization. Client bytes cannot choose infrastructure, credentials, quotas, stores, or deployment policy.
- **Protocol dependency direction.** Telchar depends on `nix-worker-protocol`, never the reverse. The protocol crate can emit `tracing`, but must not own persistence, scheduling, service configuration, or OTLP exporters. Unsupported operations fail closed; arbitrary worker messages cannot be safely skipped.
- **Narrow compatibility.** Normal build mode, classic input-addressed and fixed-output derivations only. No floating content-addressed derivations. Negotiation alone is not a compatibility claim; consult the pinned real-client evidence in `docs/compatibility.md`.
- **Gateway outputs decide success.** Backend success alone is insufficient. Every expected output must pass bounded NAR/metadata/reference/content-address validation, be imported and verified in the gateway store, and be durably recorded. Production store I/O uses typed worker protocol, not the Nix C++ ABI or shell-based import shortcuts.
- **One shared execution.** Equivalent admitted specifications coalesce. Shared states are `claimed`, `running`, `collecting`, `succeeded`, and `failed`; do not assume every completion passes through every intermediate state. The first requester owns quota; followers add no execution slot. Subject admission and backend capacity are separate gates; scheduling is round-robin across subjects and FIFO within each subject.
- **Disconnect is not execution lifetime.** Default `detach-and-finish` lets admitted work finish independently. `cancel-running` is operator policy. Logs are bounded, live-only, and not replayed; slow attachments must not block execution.
- **Recovery is exact and fail-closed.** Valid exact gateway outputs win first. Otherwise follow only persisted backend/execution identity: local is output-only, SSH reconnects to the exact target, Nomad adopts the exact job. Never migrate active work or blindly resubmit. Changing a Nomad endpoint/namespace under an in-flight backend name is unsafe.
- **Single-active ownership.** PostgreSQL lease generations fence durable writes using database time. Ownership loss closes admission and shuts down the daemon. This is not active/active or a complete HA design.
- **Migrations are append-only.** Never edit applied SQL. Add the next numbered file and register it in `persistence/migrations.rs`; preserve ledger/checksum/future-version validation. Every added mutable durable table needs its ownership trigger in the same migration. Add migration and restart/fencing coverage.
- **Bounded, secret-free control plane.** Keep NAR bodies, secret material, signatures, and logs out of PostgreSQL. Preserve transfer/time/resource bounds and low-cardinality metrics. Frontend stdout carries protocol bytes, never diagnostics. Secrets belong in protected files, not world-readable generated Nix configuration.
- **Keep product boundaries.** No automatic build retries, native TLS termination, binary-cache service, hostile multi-tenancy, or arbitrary job scheduling. Public WSS requires operator-owned TLS termination.

## Development and validation

Prefer the flake development shell. It supplies Rust/Cargo/Clippy/rustfmt, Nix, PostgreSQL, OpenSSH, and supply-chain tools. It sets `TELCHAR_NIX` and `TELCHAR_NIX_BIN` to pinned Nix.

For a feature or bugfix: write a failing behavioral test, run it to confirm the failure, make the smallest implementation change, and rerun. Test real behavior rather than source-text presence or a mock's own behavior. Protocol changes need exact-byte contracts plus real-Nix evidence; durable-state changes need failure and recovery coverage. Documentation-only changes need reference/content validation, not artificial behavioral tests.

### Focused checks

```bash
# Protocol library unit/property tests
nix develop -c cargo test --locked -p nix-worker-protocol --lib

# Example integration suite; substitute the owning test target
nix develop -c cargo test --locked -p telchar --test shared_build_recovery -- --test-threads=1

# Allocation worker
nix develop -c cargo test --locked -p telchar-nomad-worker -- --test-threads=1
```

Most gateway integration suites live in `crates/telchar/tests/` and are named after their behavior. Large suites such as `operation_dispatch`, `service_config`, `nomad_backend`, and `ipc_frontend` have root `.rs` targets plus focused subdirectories. Shared PostgreSQL/admitted-request helpers live in `tests/support/` inside the gateway crate. Exporter tests live in `crates/telchar-telemetry/tests/`; gateway and worker process telemetry tests live in their respective crates' `tests/telemetry.rs`. Their shared OTLP receiver lives in repository-root `tests/telemetry/collector.rs`. Real-Nix fixtures live in `crates/telchar/src/fixture/`; protocol contracts in `crates/nix-worker-protocol/tests/`; worker tests in `crates/telchar-nomad-worker/tests/`.

### Workspace and flake checks

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo check --locked --workspace
nix develop -c cargo clippy --locked --workspace --all-targets -- -D warnings
nix develop -c cargo test --locked --workspace -- --test-threads=1
NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build

nix build --no-link .#telchar .#telchar-nomad-worker
# Example real stock-client VM contract
nix build --no-link .#checks.x86_64-linux.nixos-fixed-output-local
```

Run available language-server diagnostics before builds. Inspect all test output; report failures and skipped/unrun checks explicitly. Never claim an evaluation-only check ran builds or integration tests.

Important distinctions:

- Full process/PostgreSQL integration authority is the serial workspace Cargo command above. `nix/checks/rust.nix` runs sandbox-compatible library tests, not the full integration suite.
- Run real-Nix/PostgreSQL fixtures as a non-root user with process/socket permissions. Helpers start isolated stores and databases; do not redirect them at production state. Nix fixtures also hold a cross-process lock; waiting on another test process is not necessarily a hung test. Do not bypass fixture locking to force parallelism.
- Some tests are deliberately ignored for private-store namespace or helper-process reasons. Reasons live beside the tests. Do not blanket-enable, add ignores, or remove tests to get green output.
- VM checks require a VM-capable Nix builder and can be expensive. Pick checks from `nix/checks/nixos.nix` and its imported modules for the changed boundary.
- CI authority is `.github/workflows/ci.yml`. Curated release verification is `./scripts/check-release.sh`: Rust checks, security scans, packages, OCI and selected VM contracts. It is not synonymous with building every flake check.
- Service boundary: `nix build --no-link .#checks.x86_64-linux.nixos-service-boundary`. Provider credential helper tests belong to telchar-gateway. Release tooling tests: `nix develop -c scripts/test-prepare-release.py`.

## Deployment and release pitfalls

- Never share a local client workload's Nix store with its Telchar gateway: recursive store locking can deadlock builds. Gateway, PostgreSQL, GC roots, and spool persistence must be considered together for recovery.
- `TELCHAR_CONFIG` selects explicit service TOML; `TELCHAR_GATEWAY_STORE_URI` selects the operator-owned daemon socket. `store/runtime.rs` captures store configuration once and passes explicit dependencies. Debug-only `TELCHAR_TEST_*` helpers are not deployment APIs.
- `nixosModules.telchar`/`default` is the only service module. PostgreSQL provisioning, host trusted-user changes, retained GC-root directories, SSH ingress, and credential acquisition belong to the deployment. `lib.sshForcedCommand` exposes the authenticated stdio adapter. `examples/nixos/` contains explicit host and AWS/Vault compositions; none are public module exports. Deployment-owned tests live in telchar-gateway. Nomad backend token consumption/reload, submission, callbacks, and recovery remain product tests in Telchar.
- SIGHUP supports transactional static SSH inventory changes and contents-only rotation of existing Nomad token files, not arbitrary configuration hot reload. Existing executions retain exact selected targets.
- `telchar operator` is read-only, but not uniformly offline: `config-check` needs no database, other commands need an existing schema, and `backends` actively probes SSH/Nix readiness. Do not use daemon startup as an inspection command; it migrates and acquires ownership.
- `nix/packages.nix` defines packages and four OCI archives: gateway, Nix-daemon sidecar, Nomad worker, SSH ingress. Keep runtime UID/GID, credential and volume ownership, and frontend authorization aligned.
- Versions use `YYYY.M.PATCH`. `scripts/prepare-release.py` coordinates the four crate manifests, workspace and fuzz lockfiles, and `nix/packages.nix`; do not bump only one authority. Publication is manually approved and exact-version-only, with no moving `latest` tag. Do not publish or run version-changing release tooling for routine development.

Keep this guide focused on stable boundaries and verified commands. Update it when those change; keep detailed designs, configuration examples, and compatibility matrices in their owning documents.
