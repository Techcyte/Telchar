# Telchar Implementation Plan

**Status:** In progress

This plan decomposes Telchar into Ralph-compatible tasks. Each task is intended to produce one small, reviewable behavior or one recorded architecture decision. Work must follow task order and gate dependencies.

## Ralph execution contract

Ralph owns implementation readiness, not acceptance. For security, protocol, identity, IPC, persistence, concurrency, scheduling, resource-boundary, and release-gate work, a fresh-context reviewer or parent review must inspect the actual production path and rerun independent evidence before the master task is checked. Implementation-authored parsers, fixtures, grep validators, and documentation are supporting evidence only.

Before executing any packet, validate that every dependency names an earlier task, completed-task evidence still matches current paths and commands, the packet status matches the master plan, and the working copy contains no unexplained changes. A packet stops instead of repairing unrelated state.

For every implementation or bug-fix task:

1. Mark exactly one task in progress in the active Ralph task file.
2. Write the smallest failing test that proves the requested behavior.
3. Run the narrow test and record the expected failure.
4. Implement only enough behavior to pass.
5. Run the narrow test and record success.
6. Run the task's broader verification command.
7. Keep expected errors captured and asserted; passing output must remain clean.
8. Record commands, working directory, relevant environment, and output summary.
9. Commit a logical changeset with `jj` before starting the next task.
10. Do not cross a phase gate until every gate item has rerunnable evidence.
11. Run the pinned formatter; never hand-edit formatter output or substitute host/LSP formatting.
12. Include at least one test through the live production composition path when the task changes a process, protocol, security, persistence, or concurrency boundary.
13. Report `<promise>IMPLEMENTATION_READY_FOR_REVIEW</promise>` only after narrow and broader checks pass; do not mark reviewer-owned master-plan acceptance or choose the next critical packet.

Independent review must state the supported behavior precisely, distinguish test-only observers from production code, disposition every finding, and rerun affected gates. Broad reviews use narrow lanes and a synthesis deadline; a stalled, killed, empty, or self-authored review is not acceptance evidence.

Decision and research tasks do not invent production behavior. They must produce an ADR, compatibility record, captured trace, threat model, or test fixture. If evidence does not support a decision, mark the dependent work blocked.

External boundaries use real components: real Nix, Nix stores, OpenSSH, PostgreSQL, SSH builders, Nomad, and cache fixtures. Do not substitute SQLite for PostgreSQL and do not test mocked repositories that merely replay desired behavior.

## Task template

```markdown
TASK-ID Short imperative subject
Depends on: earlier TASK-ID or none
Outcome: one externally observable behavior or one recorded decision
Red: failing test or evidence gap to establish first
Verify: exact rerunnable command
Evidence: paths and output facts to record
```

## Gate 0 — Reproducible project baseline

### Repository policy and tooling

- [x] T001 Record initial supported deployment assumptions
  - Depends on: none
  - Outcome: ADR records Linux-first, single-active daemon, OpenSSH forced-command frontend, PostgreSQL, TOML configuration, dedicated gateway host system store, and mutually trusted authenticated client store domain. It states that database interchangeability is not an initial goal.
  - Red: list contradictions between ADR and `telchar-design.md`.
  - Verify: repository documentation consistency check.
  - Evidence: ADR path and resolved contradictions.

- [x] T002 Add Nix flake inputs
  - Depends on: T001
  - Outcome: `flake.nix` and `flake.lock` pin nixpkgs and expose the exact initial Nix package.
  - Red: clean checkout lacks a reproducible Nix package version.
  - Verify: `nix flake metadata` and pinned `nix --version` command.
  - Evidence: locked revision and version output.

- [x] T003 Add Cargo workspace skeleton
  - Depends on: T002
  - Outcome: workspace contains `crates/nix-worker-protocol` library and `crates/telchar` binary; Telchar uses the protocol crate through a path dependency.
  - Red: flake check expecting both workspace packages fails before they exist.
  - Verify: `nix develop -c cargo build --workspace --locked`.
  - Evidence: package names, dependency edge, binary path, and clean build summary.

- [x] T004 Pin Rust toolchain in development shell
  - Depends on: T003
  - Outcome: flake development shell provides fixed `rustc`, `cargo`, formatter, linter, and test dependencies.
  - Red: version assertion fails outside selected toolchain.
  - Verify: `nix develop -c rustc --version` and `cargo --version`.
  - Evidence: pinned versions.

- [x] T005 Add formatting check
  - Depends on: T004
  - Outcome: canonical command rejects unformatted Rust.
  - Red: intentionally unformatted fixture or branch proves check fails.
  - Verify: `nix develop -c cargo fmt --check`.
  - Evidence: failing and passing summaries.

- [x] T006 Add lint check
  - Depends on: T004
  - Outcome: canonical Clippy command treats warnings as failures.
  - Red: lint fixture proves command detects a warning.
  - Verify: `nix develop -c cargo clippy --all-targets --all-features -- -D warnings`.
  - Evidence: failing and passing summaries.

- [x] T007 Add unit-test command
  - Depends on: T003
  - Outcome: canonical unit-test command runs from flake environment with pristine output.
  - Red: one failing skeleton test demonstrates harness operation.
  - Verify: `nix develop -c cargo test --lib --locked`.
  - Evidence: failing then passing test names.

- [x] T008 Add repository check aggregator
  - Depends on: T005, T006, T007
  - Outcome: one flake check runs formatting, lint, and unit tests.
  - Red: one constituent failure makes aggregate fail.
  - Verify: `nix flake check`.
  - Evidence: aggregate output summary.

- [x] T009 Document development commands
  - Depends on: T008
  - Outcome: README links design and plan, states project status and trust boundary, and documents exact bootstrap/check commands.
  - Red: documentation checklist identifies missing commands or assumptions.
  - Verify: run every documented command from clean shell.
  - Evidence: README paths and command results.

### Bootstrap observability

- [x] T009A Define telemetry contract
  - Depends on: T009
  - Outcome: ADR defines `tracing` instrumentation, OpenTelemetry logs/metrics/traces, OTLP gRPC export, local formatting, resource attributes, correlation fields, cardinality/redaction policy, bounded failure behavior, and exporter ownership in the Telchar service crate.
  - Red: design or dependency review finds an application path with no signal/correlation policy or permits exporter setup inside `nix-worker-protocol`.
  - Verify: telemetry-contract documentation check.
  - Evidence: ADR path and resolved contradictions.

- [x] T009B Add tracing and OpenTelemetry dependencies
  - Depends on: T009A
  - Outcome: workspace pins compatible `tracing`, subscriber, OpenTelemetry API/SDK, OTLP exporter, logs, metrics, and trace dependencies; `nix-worker-protocol` may use `tracing` but cannot depend on exporter SDK crates.
  - Red: workspace dependency-boundary test reports missing signal support or exporter dependency in protocol crate.
  - Verify: `nix develop -c cargo check --workspace --all-features --locked` plus dependency-boundary test.
  - Evidence: versions, enabled features, and crate dependency graph.

- [x] T009C Initialize telemetry before application work
  - Depends on: T009B
  - Outcome: Telchar installs `tracing` subscriber and OpenTelemetry providers before emitting application events, configures OTLP gRPC logs/metrics/traces plus optional local formatting, and flushes providers during shutdown.
  - Red: startup test captures application event before telemetry initialization or shutdown loses buffered telemetry.
  - Verify: telemetry lifecycle integration tests.
  - Evidence: startup order, shutdown flush, and captured signals.

- [x] T009D Bound telemetry exporter failure
  - Depends on: T009C
  - Outcome: unreachable or slow OTLP endpoint cannot crash Telchar, block startup indefinitely, recurse through exporter errors, or exceed configured queue/timeout bounds.
  - Red: controlled unavailable collector causes hang, panic, unbounded retry, or recursive error output.
  - Verify: unavailable/stalled collector integration tests with bounded wall clock and pristine captured output.
  - Evidence: configured bounds, measured duration, and failure signal.

- [x] T009E Export correlated OTLP smoke signals
  - Depends on: T009C, T009D
  - Outcome: real test collector receives one structured log, metric point, and trace span sharing required service/resource attributes and request correlation fields.
  - Red: collector fixture lacks any signal or correlation assertion fails.
  - Verify: encoded OTLP gRPC smoke integration test.
  - Evidence: collector fixture, signal assertions, trace/span IDs, and request ID.

### Compatibility and provenance baseline

- [x] T010 Record initial Nix compatibility matrix
  - Depends on: T002, T009E
  - Outcome: versioned document names pinned Nix version, Lix deferred status, expected worker-protocol range, trust modes, derivation classes, and support states.
  - Red: matrix completeness test or script reports missing cells.
  - Verify: matrix validation script.
  - Evidence: exact pinned version and matrix rows.

- [x] T011 Create real-Nix test fixture shell
  - Depends on: T002, T009E
  - Outcome: fixture creates isolated client state, keys, configuration, temporary directories, and deterministic cleanup.
  - Red: fixture self-test detects leaked state before cleanup is implemented.
  - Verify: fixture setup/teardown test.
  - Evidence: created paths and post-test absence.

- [x] T017 Record rio-build reference revision
  - Depends on: none
  - Outcome: reference record identifies exact archived upstream commit reviewed for architecture and test-category research.
  - Red: reference record lacks immutable revision.
  - Verify: reference-provenance validation script.
  - Evidence: upstream URL and full commit hash.

- [x] T018 Record rio-nix licensing discrepancy and no-copy policy
  - Depends on: T017
  - Outcome: ADR records conflicting license signals and establishes that initial `nix-worker-protocol` code will not copy, translate, or mechanically adapt Rio source.
  - Red: source policy permits ambiguous copying or lacks distinction between reference and implementation evidence.
  - Verify: independent policy review checklist.
  - Evidence: cited license signals, no-copy rule, and future import decision requirements.

- [x] T019 Define `nix-worker-protocol` crate boundary
  - Depends on: T003, T018
  - Outcome: ADR and dependency check allow only reusable wire primitives, negotiation, operations, messages, activity/error frames, result types, compatibility fixtures, property tests, and fuzz targets; Telchar domain dependencies are forbidden.
  - Red: boundary test permits identity, scheduler, PostgreSQL, SSH ingress, backend, cache, or service configuration dependencies.
  - Verify: workspace dependency-boundary test.
  - Evidence: allowed responsibilities and forbidden dependency checks.

### Protocol observation prerequisites

- [x] T022 Define protocol error model
  - Depends on: T019
  - Outcome: internal errors distinguish clean EOF, truncation, size limit, unsupported operation, version mismatch, store failure, and internal failure.
  - Red: table-driven tests fail before variants exist.
  - Verify: narrow protocol error tests.
  - Evidence: tested variants.

- [x] T023 Read little-endian worker integer
  - Depends on: T022
  - Outcome: parser reads one worker integer using captured protocol encoding.
  - Red: value test fails.
  - Verify: primitive parser test.
  - Evidence: boundary values.

- [x] T024 Reject truncated worker integer
  - Depends on: T023
  - Outcome: parser returns deterministic truncation error without panic.
  - Red: truncation test fails.
  - Verify: truncation test.
  - Evidence: asserted error.

- [x] T025 Read bounded worker byte string
  - Depends on: T023
  - Outcome: parser reads declared bytes and alignment padding within configured limit.
  - Red: valid string fixture fails.
  - Verify: byte-string tests.
  - Evidence: empty, ordinary, and padded cases.

- [x] T026 Reject oversized worker byte string
  - Depends on: T025
  - Outcome: declared length above configured maximum fails before allocation.
  - Red: oversize test observes attempted acceptance.
  - Verify: oversize test.
  - Evidence: configured bound and asserted error.

- [x] T027 Reject truncated worker byte string
  - Depends on: T025
  - Outcome: truncated payload or padding returns deterministic error.
  - Red: truncation cases fail incorrectly.
  - Verify: byte-string truncation tests.
  - Evidence: payload and padding cases.

- [x] T028 Write worker integer and byte string
  - Depends on: T023, T025
  - Outcome: encoder matches captured wire fixtures.
  - Red: golden-byte comparison fails.
  - Verify: encoding golden tests.
  - Evidence: fixture hashes or bytes.

- [x] T029 Add parser property tests
  - Depends on: T024, T026, T027
  - Outcome: arbitrary primitive input never panics and respects allocation limits.
  - Red: property test exposes an unhandled case.
  - Verify: bounded property test suite.
  - Evidence: cases and seed policy.

- [x] T030 Add parser fuzz target
  - Depends on: T029
  - Outcome: fuzz target covers primitive framing and has documented bounded smoke command.
  - Red: target absent from fuzz manifest.
  - Verify: short deterministic fuzz smoke run.
  - Evidence: command and no-crash summary.

- [x] T031 Parse client worker magic
  - Depends on: T028
  - Outcome: server accepts exact pinned client magic and rejects others.
  - Red: valid/invalid magic tests fail.
  - Verify: handshake magic tests.
  - Evidence: accepted and rejected values.

- [x] T032 Emit server worker magic
  - Depends on: T031
  - Outcome: server writes exact response expected by pinned Nix.
  - Red: golden handshake output differs.
  - Verify: handshake golden test.
  - Evidence: bytes or fixture hash.

- [x] T033 Negotiate supported worker version
  - Depends on: T032
  - Outcome: server selects version within initial matrix and records negotiated features.
  - Red: version table tests fail.
  - Verify: version negotiation tests.
  - Evidence: accepted boundaries.

- [x] T034 Reject below-minimum negotiated worker version
  - Depends on: T033
  - Outcome: negotiated versions below Telchar's minimum fail deterministically; newer client versions negotiate down to Telchar's maximum, matching pinned Nix behavior.
  - Red: server continues after below-minimum negotiation.
  - Verify: version rejection and down-negotiation tests.
  - Evidence: rejected lower boundary and negotiated upper boundary. Successful negotiation is not a compatibility or support claim for untested Nix releases.

- [x] T035 Complete pinned-client stdio handshake
  - Depends on: T033
  - Outcome: real pinned Nix completes handshake against `telchar serve-stdio`.
  - Red: real-client integration test fails at handshake.
  - Verify: direct stdio integration command.
  - Evidence: client version, negotiated protocol, clean server exit.

- [x] T036 Parse worker operation code
  - Depends on: T035
  - Outcome: reusable protocol code parses operation codes using primary Nix constants without assuming request boundaries from raw byte chunks.
  - Red: operation-code fixture is unrecognized.
  - Verify: operation-code parser unit test.
  - Evidence: tested operation codes and primary source references.

- [x] T036A Inventory typed fixture-flow requests
  - Depends on: T011, T019, T036
  - Outcome: versioned manifest maps every request, response, callback, and upload flow reachable by the compatibility fixtures to exact primary Nix serializers and bounded protocol types.
  - Red: fixture-flow inventory reports an unknown or unbounded message shape.
  - Verify: observer coverage manifest validator.
  - Evidence: operation/message list, protocol-version conditions, primary source references, and explicit unsupported flows.

- [x] T036B Parse typed fixture-flow messages
  - Depends on: T025, T028, T036A
  - Outcome: `nix-worker-protocol` can parse and relay every inventoried fixture-flow message with exact operation boundaries while retaining no secret or payload body in trace records.
  - Red: golden fixtures fail before typed message parsers exist.
  - Verify: typed observer parser golden tests.
  - Evidence: per-message fixtures, bounds, and retained metadata fields.

- [x] T036C Relay current fixture flows transparently
  - Depends on: T029, T036B
  - Outcome: observer relays every message reachable by the two current concrete `store info` fixtures at exact typed boundaries, retains only approved bounded metadata, and fails closed on an untyped flow. Primary pinned-Nix evidence establishes that neither current fixture reaches a callback or upload.
  - Red: relay loses bytes, retains a string body or secret, exceeds its configured body-transfer bound, or accepts an untyped message.
  - Verify: transparent relay integration tests cover every inventoried message, byte-for-byte request/response equality, bounded transfer buffer, and unknown-flow rejection.
  - Evidence: byte equality/hash, configured transfer bound, rejected flow, sanitized telemetry, and callback/upload reachability evidence.

### Concrete classic-build fixture expansion

- [x] T036D Define exact trusted and untrusted classic fixtures
  - Depends on: T011, T036C
  - Outcome: fixture contract fixes stock-client command, deterministic input-addressed derivation, local-build prohibition, trust configuration, daemon/socket topology, store isolation, expected output, and cleanup for one trusted and one untrusted remote-build case.
  - Red: fixture checklist contains an unspecified command, input, trust boundary, topology edge, or proof that the client did not build locally.
  - Verify: fixture-contract validator and manual command review against pinned Nix documentation.
  - Evidence: exact commands, Nix configuration, derivation source/hash, machine/process roles, and expected success/failure observations.

- [x] T036E Discover candidate classic-build flows diagnostically
  - Depends on: T036D
  - Outcome: run the fixed fixtures through a disposable diagnostic capture to identify candidate operations, responses, callbacks, and uploads; diagnostic output is sanitized and explicitly cannot satisfy compatibility acceptance.
  - Red: either fixture cannot run reproducibly or diagnostic output contains an unexplained flow or retained payload/secret.
  - Verify: repeat each diagnostic fixture and compare bounded operation/frame classifications.
  - Evidence: candidate flow list, repeatability result, primary Nix source locations to investigate, and explicit non-acceptance label.

- [x] T036F Extend typed inventory for classic-build fixtures
  - Depends on: T036E
  - Outcome: versioned manifest maps every candidate fixture flow to exact pinned-Nix serializers, version conditions, bounded protocol types, approved metadata, and fail-closed behavior.
  - Red: inventory validator finds a candidate flow without an exact typed boundary or finite bound.
  - Verify: observer coverage manifest validator for both classic-build fixtures.
  - Evidence: operation/message inventory, primary source references, bounds, and unsupported flows.

- [x] T036G Parse typed classic-build messages
  - Depends on: T025, T028, T036F
  - Outcome: `nix-worker-protocol` parses every inventoried classic-build request, response, callback, and upload boundary without retaining payload bodies or secrets.
  - Red: per-message golden fixtures fail or parser accepts a malformed/oversized shape.
  - Verify: typed classic-build parser golden, truncation, and bound tests.
  - Evidence: per-message fixtures, accepted metadata, rejected malformed cases, and allocation bounds.

- [x] T036H Relay typed classic-build flows transparently
  - Depends on: T029, T036G
  - Outcome: observer streams every inventoried classic-build flow bidirectionally with bounded memory, exact byte preservation, sanitized telemetry, and deterministic failure on any untyped flow.
  - Red: relay loses bytes, buffers a complete upload, retains a body/secret, or accepts an untyped frame.
  - Verify: trusted and untrusted relay integration tests plus large payload coverage for each inventoried streaming operation.
  - Evidence: byte hashes, peak buffer bound, rejected unknown flow, and sanitized trace fields.

### Compatibility traces and protocol evidence

- [x] T012 Add typed classic-build trace capture fixture
  - Depends on: T011, T036H
  - Outcome: the transparent typed peer relays the fixed trusted and untrusted stock-Nix fixtures while capturing operation codes and bounded protocol metadata without storing secrets or payload bodies.
  - Red: either real-client trace assertion fails before the expanded typed observer is wired.
  - Verify: real-client trusted and untrusted transparent trace commands.
  - Evidence: sanitized trace artifacts and proof that every observed flow used a typed boundary parser.

- [x] T013 Capture trusted classic derivation trace
  - Depends on: T012
  - Outcome: record handshake and operation sequence for the fixed trusted classic input-addressed remote build.
  - Red: compatibility matrix cell lacks typed acceptance evidence.
  - Verify: rerun trusted trace fixture.
  - Evidence: exact fixture ID, protocol version, trust result, operation/frame sequence, and output proof.

- [x] T014 Capture untrusted classic derivation trace
  - Depends on: T012
  - Outcome: record the exact operation and response sequence used by the fixed untrusted classic input-addressed remote build.
  - Red: compatibility matrix cell lacks typed acceptance evidence.
  - Verify: rerun untrusted trace fixture.
  - Evidence: exact fixture ID, operation/frame sequence, trust negotiation, and output proof.

- [x] T015 Defer content-addressed compatibility explicitly
  - Depends on: T010
  - Outcome: compatibility matrix and protocol allowlist mark content-addressed builds unsupported for MVP until a concrete fixture, required operations, result semantics, and typed observer coverage are separately designed.
  - Red: matrix leaves content-addressed support ambiguous or implies classic fixtures cover it.
  - Verify: compatibility matrix deferral validation.
  - Evidence: explicit unsupported status, rationale, and future evidence prerequisites.

- [x] T016 Define initial worker-operation allowlist
  - Depends on: T013, T014, T015
  - Outcome: document required, optional, recognized-rejected, and unknown operation behavior.
  - Red: captured trace contains unclassified operation.
  - Verify: classifier script over trace artifacts.
  - Evidence: zero unclassified operations.

- [x] T020 Inventory independent protocol behaviors
  - Depends on: T016, T018, T019
  - Outcome: every required behavior maps to captured traffic, primary Nix source/documentation references, and an independent implementation/test task; Rio contributes only architecture or test-category notes.
  - Red: required behavior depends on Rio implementation details or lacks primary evidence.
  - Verify: protocol evidence inventory cross-check script.
  - Evidence: per-behavior evidence sources and task mapping.

### Gate 0 acceptance

- [x] T021 Verify Gate 0 from clean checkout
  - Depends on: T008, T009, T009E, T010, T016, T018, T020
  - Outcome: clean checkout enters dev shell, reports pinned versions, passes baseline checks, exports correlated OTLP smoke signals, and validates compatibility records and provenance.
  - Red: gate script reports any missing artifact.
  - Verify: `nix flake check` plus repository gate script.
  - Evidence: exact commands and clean output summary.

### Reusable NixOS integration harness

- [x] T021A Define reusable `nixosTest` topology contract
  - Depends on: T021
  - Outcome: ADR defines authoritative multi-machine integration topology, machine roles, shared helpers, service readiness, test artifacts, secrets handling, and when specialized tests extend rather than duplicate the harness.
  - Red: integration inventory finds an external boundary with no machine role, readiness rule, or artifact policy.
  - Verify: NixOS test-topology contract check.
  - Evidence: topology diagram, extension points, and mapped future integration tasks.

- [x] T021B Add reusable `nixosTest` library
  - Depends on: T021A
  - Outcome: flake exports shared NixOS test modules/helpers for Telchar packaging, stock-Nix clients, networking, OpenSSH, OTLP collection, machine startup, and failure artifact capture.
  - Red: minimal test cannot instantiate two machines through shared helpers.
  - Verify: evaluate minimal multi-machine `nixosTest`.
  - Evidence: exported test attribute, machine definitions, and evaluation result.

- [x] T021C Add baseline client-gateway integration smoke test
  - Depends on: T021B
  - Outcome: baseline `nixosTest` boots separate client and gateway machines, completes the packaged Telchar smoke oneshot, asserts virtual-network topology, and records OTLP startup output. It proves harness packaging/topology only: it does not start the production daemon command, use local IPC or OpenSSH, complete a worker handshake, or establish cross-signal correlation.
  - Red: smoke test fails before service, networking, readiness, and collector wiring are complete.
  - Verify: flake NixOS smoke-test command.
  - Evidence: machine topology, successful oneshot service, independent network assertion, and correlated telemetry artifact.

- [x] T021D Preserve deterministic NixOS test failure artifacts
  - Depends on: T021C
  - Outcome: failed integration tests retain bounded service journals, machine state, OTLP records, and driver output while successful tests clean temporary state and emit pristine output.
  - Red: controlled failure loses diagnostics, leaks secrets, or leaves unmanaged state.
  - Verify: controlled-failure artifact and cleanup test.
  - Evidence: artifact paths, redaction assertions, and cleanup proof.

- [x] T021E Wire NixOS smoke test into repository gates
  - Depends on: T021C, T021D
  - Outcome: flake checks expose a rerunnable baseline integration target, and future real-component fixtures extend the shared `nixosTest` harness instead of creating parallel orchestration systems. Whole-system authority begins only when a gate's test exercises that gate's composed production boundary.
  - Red: aggregate validation omits the smoke test or fixture policy permits duplicate harnesses.
  - Verify: `nix flake check` plus direct NixOS smoke-test command.
  - Evidence: flake attributes, aggregate output, direct command, and runtime summary.

## Gate 1 — Stdio worker-protocol proof

### Post-capture dispatch safety

- [x] T037 Reject unknown operation code
  - Depends on: T016, T021E, T036
  - Outcome: unknown code produces deterministic Nix-compatible error framing.
  - Red: client sees EOF or panic.
  - Verify: unknown-operation integration test.
  - Evidence: captured asserted client error.

- [x] T038 Reject recognized unsupported operation
  - Depends on: T016, T037
  - Outcome: allowlist rejects a known deferred operation distinctly from unknown input.
  - Red: unsupported operation is dispatched or reported as unknown.
  - Verify: unsupported-operation test.
  - Evidence: operation and asserted error class.

- [x] T038A Define protocol session resource limits
  - Depends on: T026, T035, T038
  - Outcome: ADR defines a typed `ProtocolSessionLimits` contract with a 16 MiB maximum for concurrently retained decoded metadata, streamed-payload exclusion, checked pre-allocation charging and release, a session-owned `WorkerReader<R>` sharing one budget across live typed decoders, and a 30-second progress-reset idle deadline for incomplete typed messages enforced by the Telchar transport layer.
  - Red: contract validator finds an unspecified accounting edge, decoder owner, timeout boundary, configuration owner, or cleanup behavior.
  - Verify: protocol-session-limits contract check.
  - Evidence: allocation scope, defaults, decoder ownership, timeout semantics, transport ownership, and clean failure behavior.

- [x] T039 Bound per-session protocol allocations
  - Depends on: T038A
  - Outcome: one session-owned `WorkerReader<R>` rejects decoded metadata whose concurrently retained heap capacity would exceed the configured 16 MiB default, charging with checked arithmetic before allocation and releasing charge when metadata is no longer retained; streamed payload bodies remain outside this budget and fixture-only non-retaining slice observers remain separate.
  - Red: a sequence whose concurrently retained decoded metadata exceeds the budget succeeds, or released metadata continues consuming budget.
  - Verify: session-budget and charge-release tests.
  - Evidence: configured budget, accounting transitions, rejection point, and streamed-payload exclusion.

- [x] T040 Bound protocol session idle time
  - Depends on: T038A
  - Outcome: the Telchar transport closes a session with `io::ErrorKind::TimedOut` after the configured 30-second default without forward progress inside an incomplete typed message, resets the deadline on input progress, and leaves complete-boundary idle sessions unaffected.
  - Red: stalled partial input hangs, progress fails to reset the deadline, a complete-boundary idle session expires, or resources leak.
  - Verify: injected-short-timeout integration tests with bounded wall clock and cleanup assertions.
  - Evidence: configured duration, first-byte boundary, progress reset, boundary behavior, telemetry, and descriptor cleanup assertion.

### Independent protocol behavior

- [x] T041 Implement first inventoried protocol behavior independently
  - Depends on: T020, T035
  - Outcome: implement one required behavior in `nix-worker-protocol` from captured traffic, primary Nix source/documentation, and a failing compatibility or behavior test without copying or translating Rio source.
  - Red: named compatibility or behavior test fails before implementation.
  - Verify: crate behavior tests, real compatibility test, and evidence inventory validation.
  - Evidence: primary evidence references, test result, and no-copy attestation.

- [x] T041A Define stdout-safe local telemetry routing
  - Depends on: T009A, T041
  - Outcome: the telemetry contract reserves standard output exclusively for command protocol or machine-readable result bytes and routes every locally formatted `tracing` event to standard error, while OTLP log, metric, and trace export remains unchanged.
  - Red: a live `serve-stdio` protocol test observes a textual tracing event in the worker byte stream.
  - Verify: telemetry-contract validator plus real stdio protocol and OTLP tests proving local activity telemetry on standard error, byte-transparent worker frames on standard output, and exported structured telemetry.
  - Evidence: ADR update, asserted stdout/stderr bytes, OTLP signal assertion, and unchanged bounded redaction policy.

- [x] T042 Record Rio-informed edge-case and test inventory
  - Depends on: T017, T018, T041
  - Outcome: compare Rio's architecture and test categories against current crate coverage, adding missing test ideas without copying implementation or test bodies.
  - Red: reference review identifies an untracked edge-case category.
  - Verify: reference-to-test-category checklist.
  - Evidence: categories adopted, deferred, or rejected with reasons.

- [x] T043 Implement structured error framing independently
  - Depends on: T020, T037
  - Outcome: `nix-worker-protocol` emits error and activity frames required by the pinned client using captured traffic and primary Nix references without copying or translating Rio source.
  - Red: real client reports undecodable EOF/error.
  - Verify: crate framing tests and real-client expected-error test.
  - Evidence: primary evidence references and captured clean client message.

- [x] T044 Bound structured log and error frame sizes
  - Depends on: T043
  - Outcome: oversized outbound/inbound log metadata is rejected or truncated by explicit policy.
  - Red: frame exceeds configured budget.
  - Verify: frame-bound tests.
  - Evidence: bounds and asserted behavior.

### Gate 1 acceptance

- [x] T045 Verify Gate 1 stdio protocol proof
  - Depends on: T030, T035, T038, T039, T040, T041, T042, T043, T044
  - Outcome: real pinned Nix negotiates over the production frontend/daemon path, completes `SetOptions`, and decodes framed rejection; malformed, oversized, unsupported, and unknown inputs fail cleanly. This is not a successful store-operation or build claim.
  - Red: gate script reports missing evidence.
  - Verify: protocol unit/property/fuzz-smoke and real-client stdio suite.
  - Evidence: exact commands and pristine output.

## Gate 2 — Restricted OpenSSH ingress

### Ingress decision and identity handoff

- [x] T046 Document OpenSSH process and IPC threat model
  - Depends on: T045
  - Outcome: ADR defines frontend/daemon privilege boundary, trusted metadata sources, local peer authentication, and spoofing threats.
  - Red: threat checklist exposes unspecified trust edge.
  - Verify: ADR checklist.
  - Evidence: data-flow diagram and mitigations.

- [x] T047 Prototype public-key identity handoff
  - Depends on: T046
  - Outcome: forced command receives an authenticated key identity through OpenSSH-controlled configuration or records the approach as infeasible.
  - Red: spoofing fixture can replace identity metadata.
  - Verify: real OpenSSH key-auth fixture.
  - Evidence: key fingerprint and spoof rejection.

- [x] T048 Prototype certificate identity handoff
  - Depends on: T047
  - Outcome: capture CA, key ID, and principals securely or explicitly defer certificate support.
  - Red: matrix marks certificate support unresolved.
  - Verify: real OpenSSH certificate fixture or deferral validation.
  - Evidence: authenticated metadata or recorded deferral.

- [x] T048A Approve supported authenticated identity path
  - Depends on: T047, T048
  - Outcome: ADR identifies at least one proven OpenSSH-controlled identity path for initial ingress; if none exists, block Gate 2 and add a separately reviewed ingress redesign task.
  - Red: no supported path has spoof-resistant evidence.
  - Verify: identity evidence checklist and negative spoof test.
  - Evidence: approved mechanism and deferred mechanisms, or explicit blocker.

- [x] T049 Define requester normalization
  - Depends on: T048A
  - Outcome: credential ID, audit subject, quota subject, certificate metadata, and source address normalize deterministically.
  - Red: table-driven normalization tests fail.
  - Verify: identity unit tests.
  - Evidence: public-key and certificate cases, credential-ID quota fallback, collision-free certificate identifiers, and exact component bounds.

### Frontend and local IPC

- [x] T050 Define local IPC message envelope
  - Depends on: T046
  - Outcome: versioned envelope carries trusted requester metadata, session ID, stream attachment, and bounded error data.
  - Red: `nix develop -c cargo test -p telchar --test ipc_schema --locked` initially failed because `telchar::ipc` was absent.
  - Verify: `nix develop -c cargo test -p telchar --test ipc_schema --locked` exits `0` (`2 passed`).
  - Evidence: `docs/adr/local-ipc-envelope.md`; supported version `1`; component/session/error-code limit `256` bytes; error-message limit `4096` bytes; complete envelope limit `16 KiB`; tracing rejection events.
  - Changed paths: `crates/telchar/src/ipc.rs`, `crates/telchar/src/lib.rs`, `crates/telchar/tests/ipc_schema.rs`, `docs/adr/local-ipc-envelope.md`, `TELCHAR_IMPLEMENTATION_PLAN.md`, `.ralph/P011-frontend-daemon-ipc.md`.
  - `jj` changeset: `54e147df` (`feat: define local IPC envelope`).

- [x] T051 Authenticate local frontend peer
  - Depends on: T050
  - Outcome: daemon accepts only expected local OS identity or socket credentials.
  - Red: `nix develop -c cargo test -p telchar --test ipc_auth --locked` failed because `authorize_peer` and the peer-credential fixture were absent.
  - Verify: `nix develop -c cargo test -p telchar --test ipc_auth --locked` exits `0` (`2 passed`).
  - Evidence: `docs/adr/local-ipc-peer-authentication.md`; Linux `SO_PEERCRED` via `rustix`; current kernel UID accepted; wrong UID denied with `PermissionDenied`; bounded tracing events.
  - Changed paths: `crates/telchar/Cargo.toml`, `crates/telchar/src/ipc.rs`, `crates/telchar/tests/ipc_auth.rs`, `docs/adr/local-ipc-peer-authentication.md`, `TELCHAR_IMPLEMENTATION_PLAN.md`, `.ralph/P011-frontend-daemon-ipc.md`.
  - `jj` changeset: `b49778fe` (`feat: authenticate local IPC peers`).

- [x] T051A Resolve local IPC attachment lifecycle
  - Depends on: T046, T051
  - Outcome: one authenticated Unix connection carries exactly one bounded requester envelope followed by one worker-protocol session; the connection itself binds peer, metadata, and worker bytes without a detached bearer token or attachment registry.
  - Red: accepted threat model required daemon-issued attachment state even though metadata and worker bytes already share one authenticated ordered stream.
  - Verify: ADR review plus separate-process peer, malformed-envelope, stalled-envelope, and real worker-handshake acceptance-test definitions.
  - Evidence: `docs/adr/local-ipc-frontend-attachment.md`, `docs/adr/local-ipc-envelope.md`, and `docs/adr/openssh-process-ipc-threat-model.md`; session ID is correlation metadata only; duplicate-request suppression remains later durable request-state work.

- [x] T052 Connect `serve-stdio` frontend to daemon
  - Depends on: T051A
  - Outcome: `serve-stdio` normalizes the OpenSSH-controlled public-key fingerprint, sends one bounded requester envelope, and forwards one worker stream to a separate daemon process without owning scheduler, database, or store state.
  - Red: separate-process acceptance initially failed because the daemon command did not exist and `serve-stdio` still handled worker protocol directly.
  - Verify: `nix develop -c cargo test -p telchar --test ipc_frontend --locked` exits `0` (`6 passed`), and flake-pinned Nix completes `ssh-ng://` handshake/error tests through the separate daemon.
  - Evidence: distinct frontend/daemon PIDs; peer authorization before envelope decode; one authenticated connection binds envelope and worker bytes; malformed, oversized, frontend-error, stalled, and capacity-excess connections fail closed; rejected peers do not terminate persistent listener availability; valid sessions remain available concurrently.
  - Changed paths: `crates/telchar/src/main.rs`, `crates/telchar/src/ipc.rs`, `crates/telchar/src/session.rs`, `crates/telchar/tests/ipc_frontend.rs`, `crates/telchar/tests/operation_dispatch.rs`, `crates/telchar/tests/stdio_handshake.rs`, `docs/adr/local-ipc-frontend-attachment.md`, `docs/adr/local-ipc-envelope.md`, `docs/adr/openssh-process-ipc-threat-model.md`.
  - `jj` changesets: `a80a8dfa`, `5708d98c`, `35ca70ff`, `7874c9f9`, `563a99ea`.

- [x] T053 Bound frontend buffering
  - Depends on: T052
  - Outcome: slow daemon or client cannot cause unbounded frontend memory or unbounded accepted-session threads.
  - Red: the initial relay helper did not cover the production frontend lifecycle, and the first persistent daemon admitted an unbounded thread per accepted connection.
  - Verify: IPC buffer, separate-process lifecycle, stalled-envelope concurrency, and bounded-capacity tests pass with warnings-denied Clippy.
  - Evidence: one fixed 16 KiB stack buffer per relay direction; kernel/socket backpressure; request EOF/error half-closes daemon input; response EOF terminates frontend; 16 KiB observed maximum; 64-session default bound with prompt excess rejection; daemon-created 0700 runtime directory, refusal of insecure pre-existing directories without mutating them, 0600 socket, non-socket refusal, and shutdown cleanup.
  - Changed paths: `crates/telchar/src/main.rs`, `crates/telchar/src/ipc.rs`, `crates/telchar/tests/ipc_buffer.rs`, `crates/telchar/tests/ipc_frontend.rs`, `docs/adr/local-ipc-buffering.md`.
  - `jj` changesets: `088c7f08`, `2a99cbf9`, `7874c9f9`, `95760074`, `30a97c71`, `f0b37463`, `563a99ea`.

### SSH restrictions

- [x] T054 Generate isolated OpenSSH fixture
  - Depends on: T048A, T052
  - Outcome: NixOS or isolated sshd fixture generates host/client keys and forced-command configuration reproducibly.
  - Red: fixture boot/connect test fails.
  - Verify: fixture start/connect/cleanup command.
  - Evidence: ports, generated paths, cleanup.

- [x] T055 Complete worker handshake through `ssh-ng://`
  - Depends on: T054
  - Outcome: pinned stock Nix completes supported handshake through real OpenSSH.
  - Red: integration test fails at transport or protocol boundary.
  - Verify: `ssh-ng://` handshake test.
  - Evidence: client URI, negotiated protocol, request identity.

- [x] T056 Reject arbitrary SSH command
  - Depends on: T054
  - Outcome: requested shell command is replaced by Telchar forced command.
  - Red: arbitrary command executes.
  - Verify: negative SSH command test.
  - Evidence: asserted denial.

- [x] T057 Reject SSH PTY
  - Depends on: T054
  - Outcome: PTY allocation fails.
  - Red: PTY succeeds.
  - Verify: negative PTY test.
  - Evidence: asserted OpenSSH result.

- [x] T058 Reject SSH TCP forwarding
  - Depends on: T054
  - Outcome: local, remote, and dynamic forwarding are disabled.
  - Red: forwarding listener or connection succeeds.
  - Verify: forwarding negative tests.
  - Evidence: all denied modes.

- [x] T059 Reject SSH agent and X11 forwarding
  - Depends on: T054
  - Outcome: agent and X11 forwarding are unavailable.
  - Red: forwarded socket/display appears.
  - Verify: forwarding environment negative tests.
  - Evidence: absence assertions.

- [x] T060 Ignore client-supplied identity environment
  - Depends on: T049, T054
  - Outcome: spoofed environment cannot alter normalized requester; the accepted public-key path carries credential/audit/quota metadata through IPC, while source-address and any future certificate metadata require explicit authenticated schema coverage before use.
  - Red: spoof fixture changes requester.
  - Verify: identity spoof integration test.
  - Evidence: trusted requester remains unchanged.

### Gate 2 acceptance

- [x] T061 Verify Gate 2 restricted ingress
  - Depends on: T048A, T055, T056, T057, T058, T059, T060
  - Outcome: shared multi-machine `nixosTest` runs real stock Nix through real OpenSSH, the production forced-command frontend, authenticated local IPC, and the separate daemon; identity is trustworthy and prohibited SSH features fail.
  - Red: gate script reports missing negative test.
  - Verify: complete OpenSSH integration suite.
  - Evidence: exact command and pristine output.

## Gate 3 — Gateway store and local vertical slice

### Store boundary

- [x] T062 Document dedicated gateway-store ownership
  - Depends on: T061
  - Outcome: ADR specifies service account, daemon interaction, privileges, GC ownership, and no unrelated host workloads.
  - Red: privilege checklist exposes unspecified operation.
  - Verify: ADR checklist.
  - Evidence: required permissions and trust boundary.

- [x] T063 Create real Nix store test fixture
  - Depends on: T062
  - Outcome: reproducible fixture provisions known store state and cleans it safely.
  - Red: fixture leaves path/database/process residue.
  - Verify: setup/build/teardown self-test.
  - Evidence: pre/post state.

- [x] T064 Query path validity
  - Depends on: T063
  - Outcome: store adapter reports one valid and one invalid path through real Nix.
  - Red: integration test cannot distinguish paths.
  - Verify: real-store validity test.
  - Evidence: tested store paths.

- [x] T065 Query path metadata
  - Depends on: T064
  - Outcome: adapter returns NAR hash, size, references, deriver, and content-address metadata required by protocol target.
  - Red: metadata test lacks expected fields.
  - Verify: real-store metadata test.
  - Evidence: asserted fields.

- [x] T066 Import one validated NAR with path metadata
  - Depends on: T065
  - Outcome: after Telchar validates the streamed NAR against the client-declared NAR hash, size, path, references, deriver, and content-address metadata, Nix `Store::addToStore(ValidPathInfo, Source)` accepts it and the real gateway store reports matching registered path info.
  - Red: import integration test either bypasses pre-registration validation or registers metadata differing from the declared envelope.
  - Verify: validated real-store import test through the flake-pinned private Nix helper.
  - Evidence: declared metadata, computed metadata, helper invocation, imported path, and registered metadata.

- [x] T066A Parse and hash one streamed NAR before registration
  - Depends on: T065
  - Outcome: bounded-memory staging consumes exactly one NAR, computes SHA-256 and byte size over the NAR serialization, rejects trailing/truncated/malformed input, and produces a promotion source without trusting `nix-store --import` to validate classic input-addressed content.
  - Red: a payload mutation can reach authoritative registration without a hash mismatch.
  - Verify: fixed NAR stream parser/hash tests plus malformed/trailing-input cases.
  - Evidence: computed hash/size, bounded buffer maximum, and rejection offset.

- [x] T067 Reject corrupt NAR before authoritative registration
  - Depends on: T066A
  - Outcome: content or declared-metadata mismatch fails before the authoritative gateway store receives the object; no valid path registration or partial promoted state remains.
  - Red: mutated fixture payload imports successfully through legacy `nix-store --import`, proving that import alone is not validation.
  - Verify: hostile mutation test against staged validation followed by authoritative-store validity query.
  - Evidence: mutation location, computed-versus-declared mismatch, and invalid authoritative path.

- [x] T068 Export one valid path as NAR
  - Depends on: T066
  - Outcome: adapter streams valid path and metadata back to caller while independently confirming streamed NAR hash and size against registered path info.
  - Red: exported NAR differs from store content or registered metadata.
  - Verify: streamed export hash/size and round-trip test.
  - Evidence: content, computed hash/size, and registered metadata equality.

- [x] T069 Bound NAR transfer bytes
  - Depends on: T066, T068
  - Outcome: inbound and outbound transfers obey configured per-object and per-session limits.
  - Red: over-limit transfer succeeds.
  - Verify: transfer-limit tests.
  - Evidence: limits and rejection points.

- [x] T069A Bound transferred object counts
  - Depends on: T066, T068
  - Outcome: per-session and global object-count budgets reject excess uploads/downloads before registration or unbounded bookkeeping.
  - Red: sequence above configured count succeeds.
  - Verify: object-count admission tests.
  - Evidence: limits and rejection point.

- [x] T069B Enforce transfer rate policy
  - Depends on: T066, T068
  - Outcome: configured transfer-rate policy throttles or rejects sustained excess traffic without unbounded buffering.
  - Red: controlled sender exceeds policy without throttle/rejection.
  - Verify: time-bounded transfer-rate integration test.
  - Evidence: configured rate and observed behavior.

- [x] T070 Enforce gateway disk reserve
  - Depends on: T063
  - Outcome: new transfer/build admission fails before configured free-space reserve is crossed.
  - Red: low-space fixture admits work.
  - Verify: disk-reserve test using controlled filesystem fixture.
  - Evidence: reserve and asserted rejection.

### Minimum durable request and lease state

- [x] T070A Add minimum PostgreSQL migration runner
  - Depends on: T063
  - Outcome: Gate 3 daemon applies ordered PostgreSQL migrations for sessions, requests, attachments, and store leases transactionally.
  - Red: empty PostgreSQL database lacks minimum lifecycle schema.
  - Verify: real PostgreSQL migration integration test.
  - Evidence: PostgreSQL version, schema version, and tables.

- [x] T070B Persist minimum protocol session
  - Depends on: T070A
  - Outcome: domain-specific session state operation persists session ID, requester reference, and open/closed state across process restart.
  - Red: session round-trip/restart test fails.
  - Verify: real PostgreSQL session state-operation test.
  - Evidence: persisted fields and transaction boundary.

- [x] T070C Persist minimum build request
  - Depends on: T070B
  - Outcome: accepted build has durable immutable request identity before leases or execution.
  - Red: accepted request disappears after restart.
  - Verify: request persistence test.
  - Evidence: request row and identifier.

- [x] T070D Persist minimum request attachment
  - Depends on: T070C
  - Outcome: protocol session attachment is durable and distinct from request state.
  - Red: detach/restart test conflates session and request.
  - Verify: attachment persistence test.
  - Evidence: attached/detached states.

### GC leases

- [x] T071 Define store lease record
  - Depends on: T070C
  - Outcome: durable PostgreSQL lease identifies request/publication owner, path, purpose, and release state behind domain-specific lease operations.
  - Red: schema and operation tests fail.
  - Verify: real PostgreSQL migration and lease-operation tests.
  - Evidence: fields, constraints, and transaction ownership.

- [x] T072 Acquire derivation lease on accepted build
  - Depends on: T071
  - Outcome: accepted request roots derivation before queue visibility.
  - Red: state transition lacks root.
  - Verify: transaction integration test.
  - Evidence: request and root records.

- [x] T073 Acquire complete input-closure leases
  - Depends on: T072
  - Outcome: accepted request roots every required input path.
  - Red: closure fixture contains unleased path.
  - Verify: closure lease test.
  - Evidence: exact closure set.

- [x] T074 Preserve leased paths across GC
  - Depends on: T073
  - Outcome: real GC cannot remove queued/running request inputs.
  - Red: GC removes fixture path.
  - Verify: real-store GC test.
  - Evidence: path valid before/after GC.

- [x] T075 Release request leases transactionally
  - Depends on: T074
  - Outcome: terminal cleanup releases only eligible request roots after delivery/detachment policy.
  - Red: early release or leaked lease test fails.
  - Verify: lifecycle lease tests.
  - Evidence: state and root transitions.

### Build operation and local backend

- [x] T075A Add one-system deployment configuration
  - Depends on: T061
  - Outcome: daemon configuration requires exactly one Nix system and bounded supported-feature set; startup rejects an empty or multi-system envelope, and admission rejects mismatched build requests before store mutation.
  - Red: configuration or admission test accepts multiple systems or a mismatched request.
  - Verify: configuration parsing and admission-boundary tests.
  - Evidence: configured system, feature set, and rejection result.

- [x] T076 Parse supported derivation build operation
  - Depends on: T016, T065, T075A
  - Outcome: gateway normalizes the stock-Nix Gate 3 `BuildDerivation` operation into `BuildRequest` without backend objects.
  - Red: bounded `BasicDerivation` contract and Gate 3 request fail to parse.
  - Verify: `build_derivation_contract`, `build_request`, and focused operation-dispatch tests.
  - Evidence: normalized derivation path, outputs, inputs, system, builder, arguments, environment, and mode.

- [x] T077 Reject unsupported build option
  - Depends on: T076
  - Outcome: unsafe or unsupported client option fails deterministically.
  - Red: option passes through silently.
  - Verify: build-option allowlist test.
  - Evidence: rejected option and error.

- [x] T078 Normalize supported build options
  - Depends on: T077
  - Outcome: allowed options map to explicit internal values and defaults.
  - Red: table-driven option tests fail.
  - Verify: option normalization tests.
  - Evidence: supported set.

- [x] T079 Define local execution request
  - Depends on: T076, T078
  - Outcome: local executor receives derivation, system/features, allowed options, closure references, request ID, and cancellation token.
  - Red: schema tests fail.
  - Verify: request schema tests.
  - Evidence: required fields.

- [x] T080 Execute one derivation in gateway store
  - Depends on: T079
  - Outcome: local backend realizes one derivation through structured process arguments or Nix API, without shell interpolation.
  - Red: real local execution test fails.
  - Verify: real-store local build test.
  - Evidence: derivation and exit/result data.

- [x] T081 Capture local build log
  - Depends on: T080
  - Outcome: build log is streamed into bounded internal log channel.
  - Red: integration test cannot observe expected builder line.
  - Verify: real-build log test.
  - Evidence: asserted line and buffer bound.

- [x] T082 Apply log backpressure
  - Depends on: T081
  - Outcome: slow protocol attachment cannot grow memory beyond configured buffer.
  - Red: slow-reader test exceeds bound.
  - Verify: bounded log streaming test.
  - Evidence: observed maximum and policy.

- [x] T083 Map successful local result
  - Depends on: T080
  - Outcome: supported Nix result fields map to normalized outcome.
  - Red: result fixture lacks required fields.
  - Verify: success mapping test.
  - Evidence: mapped fields.

- [x] T084 Reject zero exit with missing expected output
  - Depends on: T083
  - Outcome: process success cannot produce Telchar success when expected output is absent.
  - Red: fault fixture reports success.
  - Verify: missing-output integration test.
  - Evidence: asserted output failure.

- [x] T085 Reject invalid imported output metadata
  - Depends on: T067, T083
  - Outcome: mismatched NAR/path metadata produces output failure.
  - Red: invalid output accepted.
  - Verify: invalid-output test.
  - Evidence: asserted validation error.

- [x] T085A Acquire request output leases before success
  - Depends on: T075, T083, T085
  - Outcome: after every expected output passes T085 validation, Telchar creates the complete output root set, commits the complete request output-lease set in one transaction, and only then makes success deliverable; derivation/input leases and roots are released without releasing the newly committed output leases.
  - Red: success can expose an unrooted output, a partial output root/lease set, or terminal request cleanup releases verified outputs before the caller can retrieve them.
  - Verify: real-store multi-output root/lease ordering, second-output root failure rollback, output-lease transaction failure rollback, and successful request cleanup tests.
  - Evidence: all output roots precede one committed output-lease set, failed batches leave zero output roots/leases, and successful cleanup leaves only active output leases.

- [x] T086 Preserve classic output trust statement in outcome
  - Depends on: T083
  - Outcome: code and docs distinguish store validation from provenance proof for input-addressed outputs.
  - Red: result documentation test finds overclaim.
  - Verify: outcome/docs consistency test.
  - Evidence: assertion location.

- [x] T086A Map classic-build operations to focused implementation tasks
  - Depends on: T016
  - Outcome: every operation required by the typed classic-build fixture inventory maps to one narrow production packet covering decoder, dispatcher, store behavior, response framing, and focused compatibility tests. Observer relay evidence cannot satisfy production coverage.
  - Red: operation coverage checker finds a required operation without a bounded implementation packet.
  - Verify: operation coverage script against allowlist and packet manifest.
  - Evidence: zero uncovered required operations and packet IDs.

- [x] T086B Add stock-Nix build walking-skeleton test
  - Depends on: T061, T086A
  - Outcome: shared `nixosTest` contains a deliberately failing acceptance path from a stock client that cannot build locally, through restricted OpenSSH and production IPC, to one local gateway-store execution and client-visible output. This test defines the vertical contract before operation implementations begin.
  - Red: test fails at the first unsupported production worker operation and records its typed operation code; local fallback is independently proven unavailable.
  - Verify: focused flake NixOS walking-skeleton command with expected failure assertion.
  - Evidence: failing operation, production process topology, and negative-local proof.

- [x] T087 Return successful build result over stdio
  - Depends on: T083, T084, T085, T085A, T086A, T086B
  - Outcome: pinned Nix client receives successful result and can copy expected output.
  - Red: the isolated Rust fixture timed out because its client and gateway stores used incompatible path identities; a first shared-store NixOS attempt then deadlocked on recursive ownership of the same output lock.
  - Verify: `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`.
  - Evidence: the authoritative NixOS Gate 3 topology now runs a stock Nix client against a test-owned SSH executable that only launches `telchar serve-stdio`, with a separate rooted client store, disabled local jobs, production authenticated IPC and gateway-store execution, exact client-readable output bytes, detached attachment, released derivation/input leases, active output lease, and exact retained output root; changeset `ab286cdb`.

- [x] T088 Return successful build result over `ssh-ng://`
  - Depends on: T087, T061
  - Outcome: stock Nix client completes same build through restricted OpenSSH.
  - Red: the Gate 3 walking skeleton originally stopped at unsupported production operations before the store/executor lifecycle was implemented.
  - Verify: `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`.
  - Evidence: flake-pinned stock Nix completes through real OpenSSH public-key authentication, restricted forced command, `serve-stdio`, authenticated IPC, and production daemon/store paths; the client copies and reads exact output bytes, and `/run/telchar/forced-command-evidence` records a server-derived `SHA256:` key fingerprint; changesets through `ab286cdb`.

- [x] T089 Prove client cannot build acceptance derivation locally
  - Depends on: T088
  - Outcome: primary fixture ensures success came from Telchar backend, not local fallback.
  - Red: the acceptance derivation fails with remote builders absent and local jobs disabled.
  - Verify: the same Gate 3 NixOS test runs negative-local before direct-stdio and OpenSSH positive lanes.
  - Evidence: `--max-jobs 0` without builders exits nonzero with a no-machine/local-build-disabled diagnostic, followed by successful direct-stdio and restricted-OpenSSH builds with exact output content; changeset `ab286cdb`.

- [x] T090 Define deployment-owned disconnect policy by lifecycle point
  - Depends on: T088
  - Outcome: ADR and validated service configuration cover upload, queued, running, collecting, and result-delivery disconnects. Running work defaults to detach-and-finish so verified outputs remain reusable; an operator may instead select cancel-running. Untrusted client bytes cannot choose the policy. First-release reattachment remains explicit.
  - Red: `deployment_config` initially failed to compile because no typed running-disconnect policy existed.
  - Verify: `nix develop -c cargo test -p telchar --test deployment_config --locked`; `nix develop -c cargo clippy -p telchar --all-targets --all-features --locked -- -D warnings`.
  - Evidence: `docs/adr/requester-disconnect-policy.md` resolves every lifecycle point; `TELCHAR_RUNNING_DISCONNECT_POLICY` defaults to `detach-and-finish`, accepts only `detach-and-finish` or `cancel-running`, fails startup on unknown/non-Unicode values, and is emitted as a bounded deployment telemetry field. No protocol/request field can select it. Changesets `753c2dba` and `df23f783`.

- [x] T091 Cancel incomplete upload on disconnect
  - Depends on: T090
  - Outcome: partial upload is discarded and resources released.
  - Red: the first focused test expected a successful frontend/daemon exit after the requester closed mid-object; actual fail-closed transport correctly exits nonzero after an attempted framed error meets the closed pipe.
  - Verify: `nix develop -c cargo test -p telchar --test operation_dispatch partial_add_multiple_to_store_failure_removes_staging_state --locked -- --exact --test-threads=1`; full `operation_dispatch` suite.
  - Evidence: requester termination during a declared 1024-byte NAR after only `partial-nar` bytes leaves the test-owned staging root empty, never invokes promotion, and records `invalid-add-multiple-to-store`; 33/33 dispatch tests pass. Changeset `d27d796d`.

- [x] T092 Apply configured running-request disconnect policy
  - Depends on: T090
  - Outcome: under the default detach-and-finish policy, transport loss durably detaches the attachment while local execution, output validation, and output leasing continue without writing to the dead transport; under cancel-running, the helper is killed and request resources are released.
  - Red: Ralph proved a detached helper failure still attempted a rejection on the dead transport; parent proved detached output-validation failure did the same after durable cleanup.
  - Verify: `nix develop -c cargo test -p telchar --test operation_dispatch --locked -- --test-threads=1`; `nix develop -c cargo test -p telchar --test deployment_config --locked`; `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`.
  - Evidence: default detach-and-finish keeps a blocking helper alive after requester loss, suppresses later logs and terminal bytes, validates the completed output, preserves its exact active output lease/root, detaches the attachment, releases derivation/input resources, and reaps the completed helper. Detached execution and validation failures suppress dead-transport rejection. Explicit cancel-running kills and reaps the helper and releases request resources. Changesets `ee0d967d`, `e9cd47ba`, and parent acceptance repair.

- [x] T093 Retain verified request outputs for a bounded retrieval window
  - Depends on: T092, T075, T085A
  - Outcome: every verified request output, connected or detached, receives a deployment-owned one-hour default local guarantee so stock Nix can retrieve it after result delivery or requester loss; cache publication remains independent; expiry durably releases leases before exact root removal.
  - Red: output leases lacked durable expiry, released output rows were excluded from reconciliation, and the daemon had no expiry maintenance lifecycle.
  - Verify: persistence, store-retention, IPC startup, operation-dispatch, and authoritative Gate 3 NixOS checks.
  - Evidence: `docs/adr/output-retention.md`; bounded configured retention, complete-set immutable deadlines, startup-before-readiness reconciliation, one 60-second maintenance thread, durable release ordering, failed-removal retry, existing stock-Nix direct/OpenSSH retrieval lanes, and private-store root-removal/GC evidence.

- [x] T093A Configure output retention duration
  - Depends on: T092
  - Outcome: `TELCHAR_OUTPUT_RETENTION_SECONDS` is typed, defaults to 3,600 seconds, accepts only inclusive range 60–86,400, fails startup otherwise, and cannot be selected by client bytes.
  - Red: focused tests initially failed to compile because `OutputRetention` and `DeploymentConfig::output_retention()` did not exist.
  - Verify: `nix develop -c cargo test -p telchar --test deployment_config --locked`; full Telchar Clippy and formatter checks.
  - Evidence: `OutputRetention` performs strict canonical decimal parsing, rejects malformed/out-of-range/non-Unicode values, exposes typed duration/seconds accessors, and adds bounded `output_retention_seconds` to `deployment.configured`. Changesets `7ca82c6d`, `bc224a26`, `e134892b`, plus parent telemetry integration.

- [x] T093B Persist immutable output retention deadlines
  - Depends on: T093A, T070A, T085A
  - Outcome: migration 0002 adds `expires_at`, backfills existing active output leases with migration time plus one hour under the explicitly approved compatibility policy, preserves already-released outputs as immediately expired, and atomically creates each complete output lease set with one immutable PostgreSQL transaction deadline.
  - Red: migration and persistence tests initially assumed one schema version and output leases had no typed deadline.
  - Verify: `nix develop -c cargo test -p telchar --test persistence --locked -- --test-threads=1`; operation-dispatch regression; Clippy and formatter checks.
  - Evidence: migration ledger version 2, exact version-1 backfill, output/non-output deadline constraints, partial expiry index, strict whole-second 60–86,400 duration validation, and one common transaction deadline for complete output batches.

- [x] T093C Release expired output leases transactionally
  - Depends on: T093B, T075
  - Outcome: a bounded keyset operation selects request-owned active output leases due at an injected time with `FOR UPDATE SKIP LOCKED`, marks the complete selected set released atomically, commits, and returns deterministic released rows as exact root-removal authority.
  - Red: no expiry transition API or boundary/cursor tests existed.
  - Verify: real PostgreSQL deadline, cursor, page-bound, state, malformed-input, and existing transaction rollback tests in the 52-test persistence suite.
  - Evidence: due rows release in lease-ID order, cursor and 256-row maximum are enforced, released rows are not selected twice, invalid input fails before connection, commit precedes return, and telemetry remains identifier-free.

- [x] T093D Reconcile expired output roots and prove later retrieval
  - Depends on: T093A, T093C, T092
  - Outcome: startup-before-readiness and one synchronous 60-second maintenance thread release expired output leases in bounded pages, then remove only exact roots authorized by released rows; failed root removal remains retryable; stock Nix retrieves connected and detached outputs before expiry without cache publication, and private-store GC can collect them after expiry/root removal.
  - Red: output expiry reconciliation API was absent; released output rows were excluded from retry pages; startup did not transition expired output leases; no maintenance thread existed.
  - Verify: `persistence` 53/53, `store_retention` 11/11, `ipc_frontend` 20/20, `operation_dispatch` 36/36, Clippy, formatter, LSP, and `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`.
  - Evidence: startup releases/removes expired output before socket readiness; startup fails closed on conflicting roots without leaking identifiers or paths; future roots remain active; committed release survives root-removal failure and retries from the durable released row; exact private-store roots preserve outputs before release and permit GC afterward; Gate 3 continues proving stock-Nix direct/OpenSSH retrieval without cache publication.

- [x] T094 Extend NixOS vertical integration fixture
  - Depends on: T021E, T088, T089
  - Outcome: shared `nixosTest` harness provisions stock client, OpenSSH ingress, daemon, gateway store, and local executor as a reproducible end-to-end topology.
  - Red: Gate 3 initially referenced the absent narrow `mkGate3Test` fixture constructor.
  - Verify: `nix flake check --no-build`; `nix build .#checks.x86_64-linux.nixos-gate-2 --no-link`; `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`; Clippy and diff checks.
  - Evidence: `tests/nixos/lib.nix` now owns the Gate 3 restricted-ingress/collector invariant while the authoritative Gate 3 script and all accepted assertions remain unchanged.

### Gate 3 acceptance

- [x] T095 Verify Gate 3 local correctness vertical slice
  - Depends on: T069, T069A, T069B, T070, T070D, T074, T075A, T084, T085, T085A, T086, T086A, T086B, T089, T091, T092, T093, T094
  - Outcome: stock client that cannot build locally receives verified output through Telchar; bounds, GC, invalid output, and disconnect policies pass.
  - Red: full `nix flake check` exposed two OTLP fixture races where smoke/artifact checks read the collector file before asynchronous export completed.
  - Verify: `nix flake check` builds and runs the full Rust, package, NixOS library, smoke, Gate 2, Gate 3, and artifact suite.
  - Evidence: collector assertions now wait for a non-empty bounded export file before reading/copying it; final `nix flake check` reports `all checks passed!`, including `nixos-gate-3-contract`.

### Pure-Rust gateway-store compatibility boundary

- [x] T095A Define the Rust Nix-daemon client boundary
  - Depends on: T095
  - Outcome: an ADR fixes a pure-Rust client over the configured Nix daemon worker protocol as the gateway-store compatibility boundary; Rust `libstore` bindings, C++ ABI/FFI, shell commands, PATH discovery, host-store fallback, and client-selected endpoints remain forbidden.
  - Red: existing root-registration client was capped at 1.25 and no accepted decision fixed the reusable/profile boundary, trust handling, operation migration, or Telchar connection ownership.
  - Verify: complete review of pinned Nix 2.34.8 handshake, post-handshake, STDERR, path-info, NAR, import, build, and daemon dispatch sources plus the accepted compatibility inventory.
  - Evidence: `docs/adr/nix-daemon-client-boundary.md` fixes protocol 1.35–1.38, typed trust/capabilities, generic-stream ownership, configured Unix endpoint, timeout/cancellation/error contracts, required operation map, resource bounds, and rejected alternatives.

- [x] T095B Implement reusable Rust Nix-daemon client negotiation and capability profile
  - Depends on: T095A, T003, T019
  - Outcome: `nix-worker-protocol` negotiates client maximum 1.38 across the supported recent daemon range 1.35–1.38, exchanges bounded feature sets when negotiated, parses bounded post-handshake daemon version/trust fields, consumes startup STDERR framing, and exposes typed negotiated version, trust, and implemented root-registration capability over a caller-owned stream. Telchar socket ownership, timeouts, cancellation, and owner death remain T095B1.
  - Red: the first packet contradicted the accepted 1.38 profile with an unchanged 1.25 golden stream; after correction, the existing client still lacked a typed profile and hostile modern-handshake coverage.
  - Verify: complete `nix-worker-protocol` suite; Clippy with warnings denied; formatter/diff checks; real trusted and untrusted project-owned private-daemon handshake.
  - Evidence: exact 1.38 client greeting and feature exchange, accepted 1.35/1.37/1.38 profiles, rejection below 1.35/wrong major/malformed trust/padding/truncation/oversize/daemon error, bounded redacted diagnostics, retained-metadata-free profile, preserved root operation bytes, and successful real Nix 2.34.8 private-daemon negotiation.

- [x] T095B1 Connect Telchar to the configured gateway daemon
  - Depends on: T095B
  - Outcome: Telchar owns a direct Unix connection only to an explicitly parsed deployment endpoint, applies fixed 30-second read/write timeouts before typed negotiation, exposes the accepted worker profile, and deterministically shuts down the socket on failed negotiation and drop.
  - Red: the parent-owned contract initially failed because `telchar::store_daemon` did not exist; hostile tests then exposed malformed-handshake connection-reset handling and non-Unicode endpoint acceptance before the implementation was corrected.
  - Verify: `gateway_daemon_connection_contract` 7/7; real trusted/untrusted private Nix-daemon profile test; full Telchar Clippy with warnings denied; formatter/diff checks; LSP/lens clean.
  - Evidence: only `unix:///absolute/path` is accepted; empty, relative, authority, query, fragment, NUL, non-Unicode, local, daemon, SSH, TCP, and unknown forms fail; missing socket has no fallback; timeout and malformed handshakes close the peer; successful drop produces EOF; public endpoint/debug/errors redact configured paths and peer bytes.

- [x] T095B2 Add typed Rust daemon path queries
  - Depends on: T095B1
  - Outcome: reusable `WorkerClient` implements bounded typed `IsValidPath` and `QueryPathInfo` operations for protocol 1.35–1.38, returning normalized classic path metadata without Telchar dependencies; `GatewayStoreConnection` exposes only those typed methods over its owned stream.
  - Red: the new exact-wire contract initially failed because the methods/type/capability did not exist; the first complete metadata test then exposed incorrect lowercase-hex validation, and attempted real private-fixture use exposed its intentionally non-`/nix/store` physical path namespace.
  - Verify: exact/hostile contract 7/7, gateway connection contract 8/8 including typed delegation, complete protocol crate green, existing real trusted/untrusted private-daemon negotiation green, full protocol/Telchar Clippy with warnings denied, formatter and diff checks, and LSP/lens clean.
  - Evidence: exact operation bytes, strict valid/invalid/present/ultimate booleans, bounded complete deriver/hash/reference/time/size/signature/content-address decode, pre-write exact logical-store path rejection, duplicate-set rejection, redacted daemon errors, and no query retry/fallback.

- [x] T095C Replace the native input-closure helper with Rust worker-protocol operations
  - Depends on: T095B2, T073
  - Outcome: production input closure is computed by deterministic reference traversal through `GatewayStoreConnection::query_path_info`, matching `computeFSClosure(roots, false, false, false)` for exact roots plus transitive references under the existing 4096-path and 1 MiB retained-path bounds.
  - Red: lifecycle tests configured only a JSON closure subprocess and failed once runtime helper use was removed; the first daemon fixture also exposed that retention and closure use distinct daemon connections and that successful lifecycle tests require derivation, input, and output root-registration traffic.
  - Verify: closure traversal unit matrix 5/5, input-root-before-lease/build integration, lease-failure rollback integration, full Telchar Clippy with warnings denied, formatter/diff checks, and LSP clean.
  - Evidence: cycles and duplicates terminate, missing/malformed paths fail closed, each discovered path is queried once, output is sorted deterministically, `TELCHAR_NIX_STORE_CLOSURE` has zero Rust runtime references, and lifecycle tests use typed worker-protocol daemon fixtures with annotated operation/field bytes.

- [x] T095D Replace the native promotion/import helper with Rust worker-protocol operations
  - Depends on: T095B, T067
  - Outcome: validated staged NARs and explicit classic metadata are registered by typed framed `AddToStoreNar` over a fresh configured gateway-daemon connection, while existing pre-registration validation and authoritative post-registration `QueryPathInfo` equality checks remain intact.
  - Red: the exact-wire contract initially lacked a typed client operation; hostile tests then required trust-gated signature bypass, fail-closed source errors, and malformed-metadata rejection before operation bytes.
  - Verify: `AddToStoreNar` contract 6/6, complete protocol suite, promotion contract 15/15 including real private-store valid/corrupt evidence, partial-upload staging cleanup, disk-reserve fail-before-body behavior, full protocol/Telchar Clippy with warnings denied, formatter/diff checks, and LSP/lens clean.
  - Evidence: op 39 metadata and framed NAR bytes match pinned Nix, untrusted connections cannot disable signature checks, no repair/ultimate/unsupported classic metadata is accepted, daemon errors are redacted, post-registration metadata equality remains authoritative, and `TELCHAR_NIX_STORE_PROMOTE` has zero Rust runtime references.

- [x] T095E Replace the native export helper with Rust worker-protocol operations
  - Depends on: T095B, T068, T085
  - Outcome: raw NAR export uses typed `NarFromPath` over a fresh configured gateway-daemon connection, bounded by authoritative `QueryPathInfo.nar_size`, then passes through existing exact-one-NAR parsing and independent hash/size verification with synchronous backpressure.
  - Red: the exact-wire client contract initially lacked `NarFromPath`; bounding by daemon EOF was rejected because pooled/real daemon connections do not close after one response, so the implementation now consumes exactly the authoritative registered NAR size.
  - Verify: `NarFromPath` exact/hostile contract 4/4, complete protocol suite, export validation/streaming contract 19/19 including real private-store byte/hash/size evidence, existing helper lifecycle regression 4/4, full protocol/Telchar Clippy with warnings denied, formatter/diff checks, and LSP/lens clean.
  - Evidence: op 38 request bytes match pinned Nix; only the registered byte count reaches the parser; malformed/truncated/trailing data, writer failure, slow-writer backpressure, object/session limits, hash/size mismatch, and backend panic remain fail-closed; `TELCHAR_NIX_STORE_EXPORT` has zero Rust runtime references.

- [x] T095F Replace the native local-build helper with Rust worker-protocol operations
  - Depends on: T095B, T084, T085A
  - Outcome: production admitted `BasicDerivation` execution uses trusted typed `BuildDerivation` op 36 through the configured gateway daemon, preserves input-addressed output, input, platform, builder, argument, environment, and normal-mode bytes, streams bounded `STDERR_NEXT` logs with synchronous backpressure, maps only `Built` and `AlreadyValid`, and verifies the exact result output set plus authoritative output existence before success.
  - Red: the outbound operation did not exist; exact-wire tests first defined the request/result/log contract. A real private fixture build reached the daemon but cannot prove production success because its physical store namespace is outside logical `/nix/store`; the production validator remains strict and the zero-exit/missing-output private-daemon case remains authoritative failure evidence.
  - Verify: typed daemon build exact/hostile contract 3/3, complete protocol suite, local-executor contract 14/14 including private-daemon missing-output rejection, gateway connection 8/8, operation dispatch 34/34 with the two already-known private-namespace path tests explicitly ignored, full protocol/Telchar Clippy with warnings denied, formatter/diff checks, and LSP/lens clean.
  - Evidence: op 36 exact request and supported result bytes, trusted-connection gate before operation bytes, bounded/redacted malformed-result and log-writer failures, cancellation/timeout socket shutdown and worker join, output equality/existence verification, zero Rust runtime references to `TELCHAR_NIX_STORE_BUILD`, and test-only helper injection isolated behind `TELCHAR_TEST_BUILD_HELPER`.

- [x] T095G Remove native-helper packaging and configuration
  - Depends on: T095C, T095D, T095E, T095F
  - Outcome: production packages, the shared NixOS fixture, local Docker fixture, flake outputs, and development shell contain no Telchar C++ helper binaries or `TELCHAR_NIX_STORE_{CLOSURE,PROMOTE,EXPORT,BUILD}` settings; the four helper sources and package outputs are deleted, while pinned Nix remains protocol-reference and fixture tooling only.
  - Red: the initial inventory found four C++ derivations, four package outputs, four installed `libexec` binaries, four dev-shell settings, four NixOS service settings, two Docker-fixture settings, and helper-dependent tests.
  - Verify: focused closure/promotion/export/output-transfer/executor/configuration/lifecycle suites, Telchar Clippy with warnings denied, formatter/diff checks, zero-name source/package/configuration inventory, `nix flake show`, and `nix build .#telchar`.
  - Evidence: package closure contains only `bin/telchar`; no `libexec/telchar` helpers exist; flake packages are `telchar`, `nix-worker-protocol`, `nix-reference`, and `default`; generated lifecycle helper scripts use explicit `TELCHAR_TEST_*` injection only; typed private-daemon tests retain the known non-`/nix/store` namespace limitations without weakening production path validation.

- [x] T095H Re-verify Gate 3 through the pure-Rust gateway-store path
  - Depends on: T095G
  - Outcome: every accepted Gate 3 protocol, store, GC, transfer, execution, output-validation, log, disconnect, and lifecycle test passes with native helpers unavailable.
  - Red: the authoritative Gate 3 fixture reached the trusted daemon build, but decoded Nix's store-relative realisation `outPath` as an absolute path and rejected the successful result; the fixture also assumed `/bin/sh`, a two-path input closure, and client-visible build-log presentation.
  - Verify: `nix build .#checks.x86_64-linux.nixos-gate-2 --no-link`, `nix build .#checks.x86_64-linux.nixos-gate-3-contract --no-link`, focused protocol/executor/connection/dispatch suites, both crate Clippy checks with warnings denied, formatter/diff checks, and `nix flake check`.
  - Evidence: `WorkerClient` reconstructs logical `/nix/store/...` paths from Nix realisation JSON and retains redacted public errors; the Gate 3 fixture seeds the runtime-shell closure into its isolated client store and verifies all twelve released input-closure leases, two derivation leases, two output leases, two detached attachments, output roots, output transfer, and OpenSSH identity evidence; native helper binaries, settings, package outputs, and processes remain absent.

## Gate 4 — Durable state, admission, and deterministic scheduling

### Durable request model

- [x] T096 Extend PostgreSQL migrations for execution state
  - Depends on: T095H, T070A
  - Outcome: ordered PostgreSQL migration 3 adds execution attempts, immutable-outcome storage, request queue state, capacity reservations, and bounded audit/quota fields transactionally.
  - Red: the real Gate 3 migration prefix applied no pending migration and could not represent attempts, outcomes, queue state, or capacity reservations.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test persistence execution_state_migration_upgrades_gate_three_rows --locked -- --exact --nocapture`; full serial persistence suite.
  - Evidence: real PostgreSQL version, schema versions 2→3, version-3 checksum ledger row, preserved session/request/attachment/lease rows, completed Gate 3 request backfill, no fabricated attempts/outcomes/reservations, state/timestamp constraints, and partial unique indexes for active attempts and reservations.

- [x] T097 Harden protocol session state operation
  - Depends on: T096, T070B
  - Outcome: the domain session operation persists bounded audit and quota subjects with immutable requester identity and transactional open/close timestamps, while Gate 3 migration rows remain preserved.
  - Red: the real PostgreSQL session round-trip test failed to compile because `open_protocol_session` accepted no audit metadata and `ProtocolSession` exposed no audit or quota fields.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test persistence open_and_read_protocol_session_persist_requested_state --locked -- --exact --nocapture`; full serial persistence suite; all Telchar targets compile with `cargo test -p telchar --no-run --locked`.
  - Evidence: exact requester reference, audit subject, quota subject, state, created/closed timestamps, pre-connection bounds enforcement, PostgreSQL constraints, atomic `RETURNING` rows, restart preservation, and redacted domain errors.

- [x] T098 Harden build request state operation
  - Depends on: T097, T070C
  - Outcome: the domain request operation persists immutable derivation identity, normalized audit/quota subjects, and typed queue metadata through one explicit PostgreSQL transaction without exposing generic CRUD.
  - Red: the real PostgreSQL request round-trip failed to compile because `create_build_request` accepted no requester subjects and `BuildRequestState` exposed no queue or audit metadata.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test persistence create_and_read_build_request_persist_immutable_state --locked -- --exact --nocapture`; full serial persistence suite; all Telchar targets compile.
  - Evidence: stable request ID, derivation path, system, accepted state with no queue timestamp, exact bounded audit/quota subjects, atomic `RETURNING` row, restart preservation, database constraints, and redacted domain errors.

- [x] T099 Harden request attachment state operation
  - Depends on: T098, T070D
  - Outcome: the domain attachment operation adds a distinct completed-delivery terminal state and timestamp while preserving detached semantics and restart-safe constraints.
  - Red: the real PostgreSQL completed-delivery test failed to compile because no delivery operation, state variant, or timestamp existed.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test persistence request_attachment_completed_delivery_survives_restart --locked -- --exact --nocapture`; full serial persistence suite; all Telchar targets compile.
  - Evidence: attached, detached, and delivered cases; monotonic terminal timestamps; mutually exclusive detach/delivery timestamps; restart round trip; repeated/competing terminal transition rejection; explicit transaction boundary; redacted domain errors.

- [x] T100 Persist execution attempt
  - Depends on: T098
  - Outcome: a typed domain operation atomically persists and reads an initial dispatching attempt with stable ID, request ordinal, idempotency key, backend, and bounded timestamps.
  - Red: the real PostgreSQL attempt test failed to compile because no attempt types or operations existed.
  - Verify: focused real PostgreSQL attempt round trip, full serial persistence suite, all Telchar targets, clippy, formatting, and diagnostics.
  - Evidence: restart preservation, unique attempt ID, unique global idempotency key, unique request ordinal, one active attempt constraint, initial-state timestamp invariants, bounded validation, and redacted domain errors.

- [x] T101 Persist immutable terminal outcome
  - Depends on: T100
  - Outcome: a typed domain operation atomically persists and reads one immutable terminal classification per attempt.
  - Red: the real PostgreSQL immutability test failed to compile because no outcome types or operations existed.
  - Verify: focused outcome restart/immutability test, full serial persistence suite, all Telchar targets, clippy, formatting, and diagnostics.
  - Evidence: primary-key ownership by attempt, bounded classification, restart preservation, rejected replacement, explicit transaction boundary, and redacted domain errors.

### Single-active ownership enforcement

- [x] T101A Define PostgreSQL singleton ownership contract
  - Depends on: T096
  - Outcome: the accepted ADR fixes advisory-lock key `0x5445_4c43_4841_5202`, a dedicated lifetime PostgreSQL connection, pre-readiness startup refusal, permanent lock-loss fencing, shutdown ordering, and bounded operator-visible telemetry without claiming high availability.
  - Red: `sh scripts/check-singleton-ownership-contract.sh` failed because the singleton ownership ADR did not exist.
  - Verify: `sh scripts/check-singleton-ownership-contract.sh`.
  - Evidence: `docs/adr/singleton-ownership.md` covers contention, database disconnect, forbidden reconnect continuity, graceful shutdown, process crash, selected fixed key derivation, side-effect fencing, takeover boundary, and sanitized telemetry.

- [x] T101B Acquire singleton ownership before service activation
  - Depends on: T101A
  - Outcome: the daemon acquires the fixed PostgreSQL advisory lock on a dedicated lifetime connection after migrations and before reconciliation, socket binding, admission, or other service activation; contention fails startup without waiting.
  - Red: the real PostgreSQL ownership test failed to compile because no singleton ownership module or operation existed.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test singleton_ownership --locked -- --test-threads=1 --nocapture`; all Telchar targets and lint checks.
  - Evidence: one owner acquires, a concurrent owner receives typed contention, replacement acquires only after connection release, daemon startup emits bounded acquired/refused telemetry, and lock values or database details are not emitted.

- [x] T101C Fence daemon after ownership loss
  - Depends on: T101B
  - Outcome: the daemon periodically checks the dedicated lifetime connection; any failure permanently closes admission by exiting the accept loop, removes the socket, emits bounded ownership-loss telemetry, and exits without reconnecting.
  - Red: the real PostgreSQL restart fixture left the daemon alive beyond the bounded deadline after its ownership connection died.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test ipc_frontend daemon_exits_and_releases_socket_after_ownership_connection_loss --locked -- --exact --nocapture`; full IPC frontend and singleton ownership suites.
  - Evidence: forced PostgreSQL connection loss produces nonzero bounded exit, removes the admission socket, releases the advisory lock for a replacement process, emits `database.singleton_ownership.lost`, and does not expose the database URL. Queue, retry, cancellation, and backend-submission paths do not exist yet and therefore cannot bypass the central admission fence.

- [x] T101D Prove singleton takeover without split brain
  - Depends on: T101C
  - Outcome: a replacement process is refused while the first daemon owns the PostgreSQL session lock and becomes ready only after the first process is stopped or fenced and PostgreSQL authoritatively releases ownership.
  - Red: the dependency review found the original task required attempt reconciliation before queue, dispatch, backend lifecycle, and restart-recovery operations exist; the process fixture then established the missing no-overlap/takeover evidence separately.
  - Verify: `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 nix develop -c cargo test -p telchar --test ipc_frontend replacement_daemon_ --locked -- --test-threads=1 --nocapture`; authoritative multi-machine ownership proof remains in T113A after restart reconciliation exists.
  - Evidence: deterministic contention refusal before socket readiness, active owner remains live during contention, forced connection-loss fencing, replacement readiness only after authoritative release, and sanitized ownership events.

- [x] T113A Prove singleton restart reconciliation without duplicate execution
  - Depends on: T109, T110, T111, T112, T113
  - Outcome: a three-machine `nixosTest` combines active-owner contention, PostgreSQL-forced ownership loss, authoritative lock release, replacement takeover, and queued/running/collecting reconciliation without duplicate backend execution or split brain.
  - Red: the first VM fixture incorrectly treated asynchronous systemd startup as synchronous contention failure; after correcting that assertion, replacement recovery failed closed because the seeded output path was absent from its gateway store.
  - Verify: `nix build .#checks.x86_64-linux.nixos-restart-reconciliation --no-link` with separate PostgreSQL, owner, and replacement VMs.
  - Evidence: contended replacement exits without a socket while owner remains active; PostgreSQL restart fences owner and removes its socket; replacement becomes ready only afterward; one queued request remains queued, one running attempt retains stable attempt/backend/idempotency identity, one collecting attempt reaches terminal success, exactly two attempts and backend rows exist, and exactly one backend result, output root, output lease, and execution outcome exist.

### State transitions and recovery

- [x] T102 Transition accepted request to queued
  - Depends on: T098, T101D
  - Outcome: a locked PostgreSQL transaction changes an accepted request to queued only when active request-owned derivation and input leases are both durable.
  - Red: the real PostgreSQL test failed to compile because no queue transition or invalid-state classification existed.
  - Verify: focused queue precondition/rollback test, full serial persistence suite, all Telchar targets, clippy, formatting, and diagnostics.
  - Evidence: missing and partial lease sets leave the request accepted, complete required lease purposes produce one monotonic `queued_at`, repeated transitions reject, and the typed authoritative row returns only after commit.

- [x] T103 Transition queued request to dispatching
  - Depends on: T100, T102
  - Outcome: one PostgreSQL transaction locks a queued request, creates its dispatching attempt and request-attributed capacity reservation, then advances the request before backend submission.
  - Red: the concurrent real PostgreSQL test failed to compile because no dispatch transition, capacity-reservation type, or invalid-state classification existed.
  - Verify: concurrent dispatch and reservation-conflict rollback tests against real PostgreSQL; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: two concurrent dispatchers produce one winner and one typed invalid-state result, exactly one active attempt and reservation persist, and reservation conflict rolls back both the attempt and request transition.

- [x] T104 Transition dispatching attempt to backend-pending
  - Depends on: T103
  - Outcome: one PostgreSQL transaction locks the dispatching attempt and request, persists the bounded backend execution ID and submission timestamp, moves the active reservation to backend-pending, and advances both lifecycle states.
  - Red: the focused test failed to compile because no backend-submission transition or backend-pending capacity phase existed.
  - Verify: real PostgreSQL success, duplicate-transition, and missing-active-reservation rollback cases; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: attempt ID/state/timestamp, request state, and reservation phase commit together; replacement submission rejects; reservation precondition failure leaves request and attempt dispatching without a backend execution ID.

- [x] T105 Transition pending attempt to running
  - Depends on: T104
  - Outcome: one PostgreSQL transaction locks the backend-pending attempt and request, moves the active reservation and its preserved units to running, records `started_at`, and advances both lifecycle states.
  - Red: the focused test failed to compile because no running transition or running capacity phase existed.
  - Verify: real PostgreSQL success, duplicate-transition, and missing-active-reservation rollback cases; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: request state, attempt state/start timestamp, and reservation phase/units commit together; repeated transition rejects; reservation precondition failure leaves request and attempt backend-pending with no start timestamp.

- [x] T106 Transition running attempt to collecting
  - Depends on: T105
  - Outcome: backend completion atomically moves the request, attempt, and active capacity reservation to collecting while leaving terminal completion and outcome absent.
  - Red: the focused test failed to compile because no backend-completion transition or collecting capacity phase existed.
  - Verify: real PostgreSQL success, duplicate-transition, missing-active-reservation rollback, and absent-terminal-outcome cases; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: `collecting_at` is monotonic after `started_at`, `completed_at` remains absent, no execution outcome exists, reservation units remain active, and failed preconditions leave the attempt running.

- [x] T107 Transition collecting attempt to terminal success
  - Depends on: T106
  - Outcome: one PostgreSQL transaction requires active request output leases and bounded nonempty result metadata, releases collecting capacity, creates the immutable success outcome, and completes both attempt and request.
  - Red: the focused test failed to compile because no terminal-success operation or persisted result metadata existed; the first implementation exposed the PostgreSQL `jsonb` parameter type boundary and was corrected to an explicit text-to-jsonb cast.
  - Verify: real PostgreSQL missing-output-lease, missing-metadata, success, immutable-replacement, active-output-retention, and terminal-record tests; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: request `completed`, attempt `succeeded` with monotonic `completed_at`, immutable `succeeded` outcome plus exact bounded JSON metadata, released capacity reservation, and still-active output lease commit together.

- [x] T107A Define terminal failure classifications
  - Depends on: T103
  - Outcome: `docs/adr/terminal-failure-classification.md` defines a closed observed-domain vocabulary without granting retry authority: build, infrastructure, admission, input, output, cancellation, and internal gateway failure.
  - Red: T108 required supported classifications, but the only transition matrix was scheduled after T108 and recovery, leaving persisted vocabulary undefined.
  - Verify: ADR review against `telchar-design.md` failure domains and later retry/reconciliation tasks.
  - Evidence: classification describes immutable terminal history; backend observations remain distinct; no class itself authorizes retry, resubmission, or reconciliation.

- [x] T108 Transition attempt to terminal failure
  - Depends on: T107A
  - Outcome: one PostgreSQL transaction terminates a dispatching, backend-pending, running, or collecting attempt using the closed failure vocabulary, bounded structured metadata, a monotonic completion timestamp, released active capacity, and matching failed request state.
  - Red: the focused test failed to compile because no terminal-failure operation existed.
  - Verify: real PostgreSQL missing/unsupported classification, supported-class vocabulary, successful dispatching failure, immutable replacement, terminal timestamps, outcome metadata, and capacity-release tests; full serial persistence suite; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: classification and metadata are immutable, request/attempt/outcome/reservation commit together, terminal attempts reject replacement, and classification records observed failure domain without granting retry authority.

- [x] T109 Recover queued requests after daemon restart
  - Depends on: T102
  - Outcome: startup reconstructs a bounded deterministic queued-request snapshot after singleton ownership and before admission readiness without mutating queue state or creating attempts.
  - Red: the restart test failed to compile because no queued recovery operation existed.
  - Verify: real PostgreSQL restart round trip plus two Telchar daemon process starts against the same durable queued request; full persistence and IPC frontend suites; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: stable `(queued_at, request_id)` ordering and exact request IDs survive PostgreSQL and daemon restart, one queued row remains, and no execution attempt is fabricated or duplicated.

- [x] T110 Recover dispatching attempt before backend ID persistence
  - Depends on: T103
  - Outcome: migration 4 adds explicit reconciling request and attempt states; startup atomically fences pre-ID dispatching attempts, releases dispatch capacity, and preserves stable backend/idempotency identity for later authoritative reconciliation without resubmission.
  - Red: the crash-point test failed to compile because no reconciling states or dispatch recovery operation existed.
  - Verify: real PostgreSQL restart test, migration upgrade test, repeated-recovery idempotency, late-submission rejection, full persistence and IPC frontend suites, all Telchar targets, clippy, formatting, and diagnostics.
  - Evidence: dispatching request/attempt/reservation become reconciling/reconciling/released in one transaction, `fenced_at` is durable, backend execution ID remains absent, idempotency key is unchanged, and a second recovery returns no work.

- [x] T110A Define persistent local executor service contract
  - Depends on: T110
  - Outcome: `docs/adr/local-executor-service.md` defines a separately running single-active local executor, PostgreSQL-backed execution registry, immutable backend/idempotency identity, bounded authenticated Unix submit/status protocol, restart ownership, and telemetry exclusions.
  - Red: T111 required a real local execution registry, but the existing synchronous in-process executor had no durable backend identity, status lookup, or work lifetime independent of the daemon.
  - Verify: contract review against singleton ownership, worker-protocol, timeout, trust, and restart-reconciliation invariants.
  - Evidence: daemon restart cannot own or terminate accepted executor work; exact duplicate submission is idempotent; conflicting identity rejects; status lookup never authorizes blind resubmission.

- [x] T110B Implement persistent local executor service
  - Depends on: T110A
  - Outcome: `telchar executor` holds a distinct fixed PostgreSQL advisory lock, serves a 1 MiB bounded peer-UID-authenticated Unix submit/status protocol, persists immutable local backend identity before responding, validates the typed execution specification against deployment policy, and durably marks accepted work running before independently owning backend execution after submitter disconnect.
  - Red: persistence tests failed to compile without registry operations; process tests had no executor command or durable status boundary; the first execution-ownership test remained accepted because submit did not start independently owned work.
  - Verify: real PostgreSQL exact/conflicting duplicate, accepted-to-running, and restart tests; multi-process service restart, status lookup, singleton contention, and submitter-disconnect execution ownership; full persistence/executor-service/executor-execution suites; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: exact submission is idempotent, conflicting identity/specification rejects, accepted state advances once to running, submit response is independent of backend duration, work remains executor-owned after the connection closes, replacement service reads durable state, contended service never creates its socket, and exactly one registry row persists.

- [x] T111 Recover backend-pending attempt
  - Depends on: T104, T110B
  - Outcome: startup reconstructs a bounded deterministic snapshot of local backend-pending attempts only when gateway request, attempt, active reservation, backend execution ID, idempotency key, and independently durable accepted executor-registry row agree.
  - Red: the real PostgreSQL restart test failed to compile because no backend-pending recovery operation existed; repeated restart then established the required stable non-mutating recovery semantics.
  - Verify: focused real PostgreSQL restart test and two Telchar daemon process starts; full serial persistence and IPC frontend suites; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: the same attempt, backend execution ID, idempotency key, submission timestamp, and active reservation survive repeated recovery; exactly one attempt and one executor-registry row persist; recovery performs no submission or state mutation.

- [x] T112 Recover running attempt
  - Depends on: T105, T110B
  - Outcome: startup reconstructs a bounded deterministic snapshot of local running attempts only when gateway request, attempt, active reservation, backend execution ID, idempotency key, and independently durable running executor-registry row agree; executor-owned work remains independent of daemon connections.
  - Red: the real PostgreSQL restart test failed to compile because no running recovery operation existed; executor ownership first required a durable accepted-to-running transition and independent execution thread.
  - Verify: submitter-disconnect process test plus real PostgreSQL running-state restart test; full serial persistence, executor-service, executor-execution, and IPC frontend suites; all Telchar targets; clippy; formatting; diagnostics.
  - Evidence: the same running attempt, backend execution, idempotency key, start timestamps, and active reservation survive repeated recovery; one attempt and one executor-registry row persist; no terminal outcome or duplicate execution is fabricated.

- [x] T113 Recover collecting attempt
  - Approved prerequisite: terminal local executor state atomically owns one immutable bounded result row, with idempotent identical writes and conflict rejection for changed terminal data.
  - Depends on: T106, T110B
  - Outcome: startup reconstructs collecting attempts only when gateway lifecycle, active collecting reservation, stable backend identity, terminal executor registry state, and immutable backend result agree; successful output validation, deterministic root identity, durable output leases, and terminal success then resume idempotently.
  - Red: the restart fixture first failed because no collecting recovery operation existed, then because output-lease creation had no exact-set idempotent recovery operation.
  - Verify: real PostgreSQL restart at the collecting crash point, repeated recovery, idempotent output-lease creation, terminal completion, full serial persistence and IPC suites, executor process tests, compile, clippy, formatting, diagnostics.
  - Evidence: one attempt, one backend result, one output lease, and one immutable execution outcome persist; repeated pre-terminal recovery returns the same work and post-terminal recovery returns none.

### Identity and admission

- [x] T114 Persist credential identity
  - Depends on: T049, T096
  - Outcome: normalized credential ID and closed OpenSSH authentication authority are immutable protocol-session audit fields; migration 7 preserves historical sessions as an explicit nullable pair rather than fabricating attribution.
  - Red: the protocol-session round-trip test failed because the persistence API, schema, and read model had no credential fields or authentication-authority type.
  - Verify: serial real-PostgreSQL persistence suite, migration-prefix/restart/concurrency tests, public-key and certificate round trips, malformed-pair fail-closed tests, compile, clippy, formatting, and diagnostics.
  - Evidence: new sessions persist `ssh-pubkey:...` with `openssh-public-key` or `ssh-cert:...` with `openssh-certificate`; historical sessions retain a paired null identity; empty, oversized, unsupported, mismatched, and partial identities reject.

- [x] T114A Implement core TOML service configuration
  - Depends on: T114
  - Outcome: strict typed core configuration loads optional `/etc/telchar/telchar.toml`, requires an explicitly selected `TELCHAR_CONFIG`, applies existing scalar environment overrides after TOML, reads the database URL through `database.url_file`, and supports bounded credential mappings with whole-map replacement through `TELCHAR_IDENTITY_MAPPINGS_FILE`.
  - Red: the focused service-configuration test failed because no `config` module or TOML loader existed.
  - Verify: service-config, deployment-config, identity, IPC frontend, executor service, and executor execution suites; all-target compilation, clippy, formatting, and diagnostics.
  - Evidence: unknown fields, unreadable explicit files, non-Unicode overrides, unsafe paths, invalid bounds, malformed mappings, and empty mapping entries fail closed; daemon, executor, and frontend consume one merged configuration; a real frontend/daemon process persists mapped audit and quota subjects.

- [x] T115 Map credential to audit subject
  - Depends on: T114A
  - Outcome: exact normalized credential-ID lookup selects a bounded configured audit subject; an unmapped public key falls back to its authenticated fingerprint and an unmapped certificate falls back to its first authenticated principal.
  - Red: the frontend integration test failed before core configuration and credential mapping existed.
  - Verify: identity table tests, strict service-config mapping tests, and real frontend/daemon process persistence test.
  - Evidence: mapped public-key ingress persists the configured stable audit subject; unmapped public-key and certificate normalization retain deterministic authenticated fallbacks; unsupported, empty, oversized, and empty-entry mappings fail closed.

- [x] T116 Map credential to quota subject
  - Depends on: T114A
  - Outcome: exact normalized credential-ID mappings allow multiple credentials to share one bounded quota subject; unmapped credentials fall back to their immutable credential ID.
  - Red: the multiple-credential process test failed before core configuration supplied quota mappings to `serve-stdio`.
  - Verify: strict service-config mapping test, identity fallback table, and real persistent frontend/daemon process tests.
  - Evidence: two distinct authenticated public keys persist one shared quota subject, while an unmapped public key persists its `ssh-pubkey:...` credential ID as quota subject.

- [x] T117 Enforce global concurrent session limit
  - Depends on: T097, T114A
  - Outcome: the configured global IPC session limit atomically admits at most the bounded number of authenticated OpenSSH protocol sessions, refuses excess sessions before durable protocol-session creation, releases capacity when a session ends, and permits later reuse.
  - Red: no real OpenSSH fixture previously held one authenticated session open while attempting another.
  - Verify: real concurrent OpenSSH process test with PostgreSQL session-state evidence, full OpenSSH ingress suite, compile, clippy, formatting, and diagnostics.
  - Evidence: with `ipc.maximum_sessions = 1`, the first SSH session is open, the second exits unsuccessfully without creating another open protocol session, closing the first reduces the open count to zero, and a third SSH session succeeds.

- [x] T118 Enforce global retained-byte limit
  - Depends on: T069, T073, T114A
  - Outcome: migration 8 records positive authoritative NAR sizes on derivation and input lease obligations; `deployment.maximum_retained_input_bytes` and `TELCHAR_MAX_RETAINED_INPUT_BYTES` bound the sum of unique active retained gateway-store paths, with exact duplicate paths charged once globally.
  - Red: concurrent real-PostgreSQL lease acquisition admitted two distinct six-byte paths against a six-byte budget because no durable size or serialized budget check existed.
  - Verify: concurrent unique-path and overflow persistence tests, full 79-test PostgreSQL suite, closure/config/deployment/frontend suites, all-target compilation, clippy, formatting, and diagnostics.
  - Evidence: two concurrent leases for one six-byte path both commit while global retained bytes remain six; two concurrent distinct six-byte paths yield exactly one committed lease and one `capacity` rejection; path-size disagreement conflicts; migration backfills historical derivation/input leases conservatively.

## MVP remaining work — focused build gateway

The MVP is one stable stock-Nix `ssh-ng` endpoint backed by a small durable build coordinator. Equivalent normal-mode derivation requests coalesce into one shared execution, each connected requester waits synchronously for that execution, and backend work continues independently of client attachment. Completed outputs live in the gateway store, so a reconnecting or retrying Nix client either attaches to the active shared build or receives the already-valid result.

The coordinator selects one compatible configured backend by exact system, required-feature subset, declaration order, and a bounded backend permit. It does not provide a general durable queue, fairness policy, priorities, automatic retries, attempt history, per-subject capacity reservations, detached-client result APIs, or cache service. Nomad owns cluster placement, pending allocations, and any infrastructure autoscaling. Existing Nix substituters and publishers such as Attic or S3-backed caches remain external infrastructure.

The durable coordinator is intentionally an extension seam rather than a reduced copy of the previous scheduler design. Future queueing, fairness, priorities, quotas, retries, or administrative controls can consume the same admitted build key, backend declarations, shared-build record, terminal result, and attachment events. Those features must add their own policy and schema only when demonstrated need exists; the MVP does not preserve dormant scheduler transitions in anticipation of them.

### Minimal backend boundary

- [x] T120 Define the minimal synchronous backend contract
  - Depends on: T118
  - Outcome: `BuildBackend` accepts a bounded `BuildExecution`, forwards logs and cancellation observations synchronously, and returns a normalized `BuildResult` with closed status and output-trust values.
  - Red: the backend contract test failed to compile because no backend-neutral module or types existed.
  - Verify: focused backend contract test, all-target compilation, clippy, formatting, diff, and diagnostics.
  - Evidence: a fixture backend observes exact request metadata, forwards a log chunk before completion, checks cancellation, and returns a trusted terminal output set without queue, scheduler, reconnect, or cache behavior.

- [x] T121 Route builds by configured system and features
  - Depends on: T120
  - Outcome: bounded `BackendTarget` declarations identify local, static SSH, and Nomad backends; `select_backend` returns the first declaration matching the exact system and containing every required feature, with no fair queue, priority, or capacity scheduler.
  - Red: the compact routing test failed to compile because no backend target, kind, or selection operation existed.
  - Verify: routing and backend contract tests, all-target compilation, clippy, formatting, diff, and diagnostics.
  - Evidence: declaration order selects local for `kvm`, static SSH for `big-parallel+kvm`, and rejects unsupported systems or features.

- [x] T121A Advertise bounded backend coordination capabilities
  - Depends on: T120, T121, T126
  - Outcome: each backend target exposes typed execution recovery (`output-only` or `adoptable`), cancellation (`connection-bound` or `explicit`), and log recovery (`live-only` or `replayable`) capabilities. Local and static SSH are output-only/connection-bound/live-only; Nomad is adoptable/explicit/live-only. Capability values come from backend kind and verified implementation, never client input or free-form operator claims.
  - Red: the focused backend contract test failed to compile because capability types and `BackendTarget::capabilities()` did not exist.
  - Verify: focused backend contract tests, all-target compilation, clippy, canonical formatting, diff, and diagnostics.
  - Evidence: exhaustive `BackendKind::capabilities()` maps local/static SSH to output-only/connection-bound/live-only and Nomad to adoptable/explicit/live-only; immutable typed accessors expose each axis without adding operator- or client-controlled capability fields. Persisted/configured disagreement remains a T127 database invariant, and coordinator log fan-out remains T128 work.

- [x] T122 Adapt the existing local executor to the minimal backend contract
  - Depends on: T120
  - Outcome: gateway-daemon, helper-process, unavailable, session, and executor-service paths use `BuildBackend`, `BuildExecution`, `BuildResult`, and `BuildStatus` directly; duplicate local-neutral request, result, status, trust, and executor abstractions are removed without changing client-visible behavior.
  - Red: the local contract test required all three local executor implementations to implement `BuildBackend`, which failed before adaptation.
  - Verify: 13 local executor tests, 3 local configuration tests, 2 backend tests, all-target compilation, clippy, formatting, diff, and diagnostics.
  - Evidence: bounded logs, cancellation, timeout, output-set validation, gateway-store verification, trusted terminal results, session framing, and executor-service persistence remain on the same execution paths behind the shared contract.

### Static SSH backend

- [x] T123 Parse and validate static SSH backend configuration
  - Depends on: T121
  - Outcome: strict `[[backends.static_ssh]]` entries supply a bounded unique name, exact system, bounded features, safe destination, fixed private identity file, and fixed known-hosts file containing at least one pinned host key.
  - Red: the focused configuration test failed to compile because `ServiceConfig` exposed no static SSH backend configuration; later hostile tests exposed missing pinned-key-content and writable-known-hosts checks.
  - Verify: 9 service configuration tests, all-target compilation, clippy, formatting, diff, and diagnostics.
  - Evidence: valid configuration produces a `BackendKind::StaticSsh` target; relative or missing files, permissive private-key mode, writable known-hosts mode, absent pinned keys, unsafe destinations, duplicate names, excessive counts, unsupported fields, and invalid target metadata fail startup.

- [x] T124 Provision a restricted static SSH builder fixture
  - Depends on: T123
  - Outcome: a real two-node NixOS VM fixture provisions a dedicated unprivileged SSH builder account whose fixed command accepts only `nix-daemon --stdio`; public-key authentication uses a pinned client-side host key and shell, PTY, local/remote TCP forwarding, agent forwarding, X11 forwarding, and user environment requests are denied.
  - Red: flake evaluation failed because no static SSH fixture harness existed; the first VM runs exposed missing forced-command execution, unwritable evidence state, command-shape assumptions, and PTY test-driver hangs before the fixture became authoritative.
  - Verify: `nix build .#checks.x86_64-linux.nixos-static-ssh-fixture --no-link -L`, flake evaluation, nixfmt, diff, and diagnostics.
  - Evidence: stock Nix `ssh-ng` successfully completes `store ping` through the restricted account while arbitrary commands and every tested forwarding or interactive channel fail; server evidence contains no forwarded agent socket or display.

- [x] T125 Execute and collect one build through static SSH
  - Depends on: T122, T124
  - Outcome: Telchar selects a compatible configured static SSH backend, stages the admitted derivation closure through typed worker-protocol NAR operations, runs `BuildDerivation` over a pinned OpenSSH connection, drains bounded SSH diagnostics and Nix build logs, imports declared outputs into the gateway store, validates the exact output set, and returns a normal trusted Nix result.
  - Configuration: `ssh_program` is an optional absolute executable override. Nix packaging embeds `${pkgs.openssh}/bin/ssh`; non-Nix builds use `/usr/bin/ssh`. No `PATH` discovery occurs.
  - Red: the backend module was absent; real VM execution then exposed duplicate worker-operation frame consumption, streaming deadlocks when one protocol session performed both sides concurrently, imported `ultimate` metadata rejection, and waiting for a persistent `nix-daemon --stdio` session after the terminal result.
  - Verify: `nix build .#checks.x86_64-linux.nixos-static-ssh-build --no-link -L`, focused worker-protocol/config/backend tests, all-target compilation, Clippy with warnings denied, canonical formatting, diff, and diagnostics.
  - Evidence: the authoritative three-node NixOS VM completes in 28.09 seconds; the remote builder receives only the forced `nix-daemon --stdio` command, the gateway imports and verifies the output, and the stock client reads the expected result.

### Durable shared-build coordinator

- [x] T126 Record the lean coordinator decision and cleanup boundary
  - Depends on: T125
  - Outcome: an ADR defines normal-mode derivation coalescing, client-independent execution ownership, five shared-build states (`claimed`, `running`, `collecting`, `succeeded`, `failed`), capability-driven restart reconciliation, connection-scoped MVP logs, and explicit exclusions for queues, retries, attempts, fairness, priorities, quotas, and capacity reservations.
  - Verify: design review against stock-Nix behavior, gateway-store authority, singleton ownership, and the three backend contracts.
  - Evidence: `docs/adr/durable-shared-build-coordinator.md` makes the gateway store authoritative for completed outputs; defines typed execution-recovery, cancellation, and log-recovery capabilities; gives Nomad deterministic adoptable identity; lets local/static SSH recover exact outputs or fail cleanly; distinguishes coordinator log fan-out from backend replay; and preserves explicit extension seams without retaining dormant machinery.

- [x] T127 Persist one shared build per equivalent derivation
  - Depends on: T121A, T126
  - Outcome: PostgreSQL atomically claims one bounded request digest per normal-mode derivation path, records the selected backend and its coordination capabilities, requires a stable backend execution ID for adoptable executions, stores bounded expected-output and terminal metadata, and permits a later client request to replace a failed build without automatic retry.
  - Red: focused real-PostgreSQL tests failed to compile because shared-build claim, lifecycle, restart-read, and failed-row replacement APIs did not exist; the first terminal update then exposed PostgreSQL parameter-type ambiguity before explicit typed casts were added.
  - Verify: serial real PostgreSQL persistence suite, all-target compilation, clippy with warnings denied, canonical formatting, diff, and diagnostics.
  - Evidence: migration 9 adds the bounded five-state `shared_builds` table and indexes; simultaneous equivalent claims yield one owner and one joiner; digest, backend, capability, execution-ID, and output disagreement fail closed; adoptable records require stable execution IDs; typed transitions enforce `claimed → running → collecting → succeeded` and failure from any nonterminal state; terminal results are immutable and bounded; active rows survive restart in deterministic order; and a later independent request atomically replaces one failed row without automatic retry or attempt history.

- [x] T128 Coalesce connected requests around one execution
  - Depends on: T127
  - Outcome: one request becomes leader, matching concurrent requests attach as in-memory followers, exactly one backend execution runs, followers receive a bounded already-in-progress message and the shared terminal result, and disconnecting requesters never own or cancel backend work.
  - Red: focused tests first failed because no process-local registry or explicit leader/follower roles existed; production integration then exposed overlapping mutable output ownership and required an explicit acquisition API rather than dual execution callbacks.
  - Verify: concurrent identical-request, follower disconnect, shared success, shared failure, later-request-after-failure, full operation-dispatch suite, all-target compilation, clippy, canonical formatting, diff, and diagnostics.
  - Evidence: one process-wide registry is shared by daemon session threads; admitted request semantics produce a derivation-path plus 32-byte SHA-256 identity; exactly one frontend executes the helper while followers receive the bounded already-in-progress frame and shared terminal result; leader and follower disconnect fixtures leave the backend execution independent of either requester; failure wakes all waiters and releases the key for a later request.

- [x] T129 Route each leader across configured backends
  - Depends on: T121, T128
  - Outcome: production execution selects a configured backend per admitted leader, honors exact system and derivation `requiredSystemFeatures`, acquires one bounded per-backend permit in declaration order, and leaves followers outside backend capacity accounting without a durable queue or Telchar capacity reservation.
  - Red: request tests first proved required feature metadata was discarded; backend tests then proved no shared permit pool existed; integration failed because production selected one executor per session and unconfigured fixtures lost their local execution path.
  - Verify: feature admission and shared identity, mixed-backend selection, busy-backend waiting, timeout, permit release, strict capacity configuration, static-SSH startup worker handshake, full operation-dispatch suite, all-target compilation, clippy, canonical formatting, diff, and diagnostics.
  - Evidence: deployment-advertised features must exactly equal the aggregate features of configured same-system backends; local and static-SSH backends have bounded explicit concurrency; leaders wait for the first compatible target while followers share the active execution; static-SSH targets complete a bounded `nix-daemon --stdio` worker handshake before the daemon accepts traffic; the default environment-only deployment retains one bounded local target for existing deployments.

- [x] T130 Reconcile active shared builds after restart
  - Depends on: T127, T129
  - Outcome: startup reads active shared builds in a bounded batch, validates their exact expected outputs in the gateway store first, compares persisted backend kind and typed capabilities with current configuration, resumes only exact durable adoptable executions, recovers completed outputs, and marks unsupported, missing, or capability-inconsistent executions failed so an ordinary later request may claim them again.
  - Red: focused recovery tests first failed because no coordinator recovery module existed; startup integration then exposed that connecting to the gateway store when no active builds exist breaks store-independent protocol fixtures, requiring a bounded active-row read before opening the worker-protocol connection.
  - Verify: completed-output precedence, output-only failure, exact adoptable monitoring, missing execution, capability disagreement, persistence 85/85, shared-build recovery 4/4, operation dispatch 37 passed/2 ignored, all-target compilation, clippy, canonical formatting, diff, and diagnostics.
  - Evidence: recovery uses persisted capability values rather than backend-kind guesses; exact output metadata can move any active state through collecting to immutable success; local/static-SSH rows without complete outputs fail; adoptable rows require an exact durable execution ID and a matching configured backend; adoption results distinguish monitoring, succeeded, failed, and missing; startup performs reconciliation after singleton ownership and static-SSH verification but before socket readiness.

- [x] T131 Consolidate durable scheduling around shared builds
  - Depends on: T127, T128, T130
  - Outcome: the live shared-build path owns one durable subject-fair queue, trusted `quota_subject` attribution, one quota allocation per coalesced leader, bounded per-subject queued and active execution limits, durable execution attempts and terminal outcomes, and backend permits; followers consume transfer limits but no additional execution allocation. Parallel dormant request/attempt/reservation lifecycles are either integrated into this path or removed.
  - Policy: the first admitted requester owns the shared build's quota allocation until terminal completion even after disconnect; matching followers receive the existing shared execution without another build charge; queue selection is round-robin across eligible quota subjects and FIFO within each subject; quota admission and backend capacity remain separate gates; no automatic retry, priority, billing, or active/active scheduler is added in this task.
  - Red: durable queue tests first lacked waiting and persisted rotation; concurrent frontends exposed follower lifecycle mutation and publication-before-terminal races; backend-capacity tests proved subject admission and permits were conflated; restart review exposed that active shared-build rows and their durable attempts could disagree without failing closed; disconnect fixtures exposed unordered simultaneous frontend handshakes.
  - Verify: subject mapping, per-subject queue bounds, active execution limits, one charge per coalesced build, owner-disconnect retention, follower non-charging, subject round-robin/FIFO ordering, durable restart recovery, allocation release on every terminal path, backend permit interaction, schema inspection, persistence 69/69, operation dispatch 40 passed/2 ignored, shared-build scheduler 4/4, scheduling 7/7, recovery 5/5, service configuration 10/10, all-target compilation, clippy with warnings denied, canonical formatting, diff, and diagnostics.
  - Evidence: PostgreSQL owns trusted first-requester quota attribution, durable queue position, the persisted fairness cursor, one admitted attempt and immutable outcome per coalesced leader, and every terminal transition; subject-scoped transactional locks enforce queued and active limits; eligible subjects rotate round-robin while each subject remains FIFO; queued ownership survives disconnect; followers create no allocation, attempt, or permit; backend permits remain a separate post-admission capacity gate; exact gateway outputs retain restart precedence while missing or divergent active attempt identity fails closed before adoption; schema 13 removes dormant request queue fields and the parallel `execution_attempts`, `execution_outcomes`, and `capacity_reservations` lifecycle while preserving request identity, attachments, leases, and local executor recovery records.

### Static SSH completion

- [x] T132 Handle static SSH timeout, failure, and shared-build recovery
  - Depends on: T125, T130
  - Outcome: authentication, worker protocol, staging, execution, collection, missing-output, timeout, and cancellation failures terminate cleanly without leaking credentials or unbounded data; restart reconciliation imports exact remote outputs when available and otherwise fails the shared build cleanly.
  - Red: hostile transport diagnostics exposed the configured destination and identity-file path; blocking descendants proved direct-child termination could hang timeout and cancellation forever; output-only restart recovery always failed even when exact outputs remained on the configured SSH builder.
  - Verify: hostile diagnostic redaction, runtime timeout, cancellation, process-group cleanup, malformed worker protocol, exact missing remote output, bounded restart timeout, recovered-output success and failure, static SSH 8/8, shared-build recovery 7/7, operation dispatch 40 passed/2 ignored, persistence 69/69, all-target compilation, clippy with warnings denied, canonical formatting, diff, and diagnostics.
  - Evidence: SSH stderr is drained concurrently through a bounded channel but replaced with a fixed transport diagnostic; runtime and recovery children use dedicated process groups that are killed and reaped on every terminal path; live backend errors become durable `backend-failure`; restart first trusts exact gateway outputs, then reconnects only to the exact configured static SSH backend, queries and imports every expected remote output through worker protocol, verifies gateway presence, and otherwise records immutable `restart-recovery-failed` without resubmission or retry.

- [x] T133 Verify the static SSH gateway
  - Depends on: T128, T129, T132
  - Outcome: pinned stock Nix clients submit identical and distinct derivations through Telchar without knowing which static SSH machine serviced them; duplicates coalesce and distinct builds fan out according to configured compatibility and permits.
  - Red: the first four-machine fixture exposed that stock Nix may send an empty `AddMultipleToStore` when it already considers an installable valid, so the fixture could not assume the derivation was present in the gateway store; distinct feature-routed builds also proved each remote Nix daemon must advertise the exact required system feature.
  - Verify: authoritative four-machine `nixosTest`, one durable attempt for two concurrent identical requests, exact forced-command connection counts per configured builder, disjoint `primary` and `secondary` feature routing, normal stock-Nix output transfer, output contents, gateway `nix-store --verify-path`, all-target compilation, clippy with warnings denied, canonical formatting, and diagnostics.
  - Evidence: the stock client connects only to Telchar over `ssh-ng`; fixture setup stages derivations into the gateway store without exercising an unsupported standalone copy path; two identical requests share one static SSH execution on the declaration-order primary backend; two distinct requests run through separate one-permit primary and secondary backends selected by exact required features; all three outputs return to the stock client and remain valid in the gateway store.

### Nomad backend

- [x] T134 Define and parse the minimum Nomad backend configuration
  - Depends on: T121, T126
  - Outcome: operator configuration supplies independently named Nomad targets with endpoint, namespace, protected credential files, system/features, generic operator-controlled task driver configuration, bounded resources, deterministic job-name scope, polling bound, runtime bound, and permits.
  - Red: strict configuration tests initially lacked Nomad targets, cross-kind backend-name validation, protected Nomad credentials, bounded generic driver configuration, and deterministic rendered jobs.
  - Verify: `nix develop -c cargo test --locked -p telchar --test service_config --test build_backend --test nomad_backend`; all-target compilation; clippy with warnings denied; canonical formatting.
  - Evidence: repeated `[[backends.nomad]]` entries support distinct clusters and drivers; validation bounds credentials, endpoints, resources, polling, runtime, driver configuration depth/count/size, and global backend names; `render_job` binds deterministic SHA-256-derived job identity to the exact backend name, namespace, system, driver, resources, and operator configuration. Changeset `033d6e74` plus backend-derived multi-system routing in `d30327f0`.

- [ ] T135 Provision a real Nomad development fixture
  - Depends on: T134
  - Outcome: reproducible Nomad server/client nodes can run, query, and clean up one isolated batch job whose deterministic identity survives a Telchar process restart.
  - Verify: fixture smoke and job-adoption tests.

- [ ] T136 Submit and monitor one durable Nomad build
  - Depends on: T122, T127, T130, T135
  - Outcome: the shared-build leader renders one deterministic batch job, submits it once, records its identity, polls until terminal state independently of client attachment, and resumes monitoring the same job after Telchar restart without blind resubmission.
  - Verify: real submission, client disconnect, daemon restart, adoption, and completion tests.

- [ ] T137 Transfer inputs, logs, and outputs for Nomad
  - Depends on: T136
  - Outcome: the allocation receives only the admitted input closure, emits bounded live logs to currently attached clients, and returns the exact declared outputs for gateway import and validation; historical log replay is not promised.
  - Verify: real private-input, bounded-log, follower-attachment, and output-collection tests.

- [ ] T138 Handle Nomad timeout and failure
  - Depends on: T137
  - Outcome: pending, allocation, task, transfer, collection, missing-job, and timeout failures produce one clean terminal shared-build failure; Telchar performs no automatic retry and leaves placement, pending work, and autoscaling interaction to Nomad.
  - Verify: focused controlled Nomad failures.

- [ ] T139 Verify the Nomad gateway
  - Depends on: T128, T129, T138
  - Outcome: pinned stock Nix clients build through Telchar without knowing the Nomad allocation; concurrent duplicates create one job, reconnecting clients attach to active work, and completed gateway outputs satisfy later requests.
  - Verify: authoritative Nomad integration test.

### MVP operations and release

- [ ] T140 Complete strict MVP service configuration
  - Depends on: T123, T134
  - Outcome: one TOML schema configures the public system/features, PostgreSQL, IPC/OpenSSH ingress, shared-build retention, backend permits, and local/static-SSH/Nomad backends; secrets use protected file references and unknown fields fail startup.
  - Verify: configuration suite.

- [ ] T141 Bound shutdown, runtime, coordination, and logs
  - Depends on: T132, T138
  - Outcome: daemon shutdown, shared-build monitoring, backend runtime, follower waiting, child processes, polling, and live logs have explicit bounded behavior; active backend work remains client-independent and optional historical log archival remains deferred.
  - Verify: shutdown, timeout, follower, polling, and bounded-log tests.

- [ ] T142 Build the reproducible package and NixOS module
  - Depends on: T140, T141
  - Outcome: flake outputs install Telchar services, restricted OpenSSH ingress, configuration, credentials, gateway Nix-daemon access, PostgreSQL coordination, and the selected backend dependencies.
  - Verify: package build and NixOS module VM test.

- [ ] T143 Document external cache and optional log-archive integration
  - Depends on: T142
  - Outcome: operator docs show ordinary Nix substituters and existing Attic, post-build-hook, or `nix copy` publication beside Telchar; they also define the post-MVP extension seam for a bounded local zstd log spool mounted on durable storage or uploaded by external tooling. Telchar implements no cache service, Redis log store, or object-storage client in the MVP.
  - Verify: tested configuration examples and explicit log-loss behavior after late attachment or restart.

- [ ] T144 Document deployment, security assumptions, and limitations
  - Depends on: T142
  - Outcome: docs cover trust, credentials, host keys, stores, PostgreSQL shared-build ownership, backend-specific restart recovery, supported backends, normal Nix retry behavior, connection-scoped logs, and explicit non-goals.
  - Verify: operator checklist.

- [ ] T145 Add one focused release verification command
  - Depends on: T133, T139, T142, T143, T144
  - Outcome: one command verifies formatting, lint, tests, package/module checks, duplicate coalescing, restart reconciliation, and stock-Nix builds through local, static SSH, and Nomad backends.
  - Verify: clean-shell release command.

- [ ] T146 Verify the MVP release candidate
  - Depends on: T145
  - Outcome: Telchar demonstrably acts as a stable Nix build gateway with one durable shared execution per equivalent derivation, compatible-backend fan-out, client-independent monitoring, and documented residual limitations.
  - Verify: immutable release report with exact commands and versions.

## Explicitly deferred work

These features require a demonstrated operational need before design or implementation:

- General durable queues, durable client attachments, and historical log resumption.
- FIFO, round-robin, fairness, priorities, non-starvation proofs, and scheduler load testing.
- Per-person, per-credential, or per-quota-subject limits and accounting.
- Automatic retry frameworks, attempt history, exact-once execution promises, and ambiguous-execution reconciliation beyond deterministic backend identity plus clean failure.
- Administrative queue/status/cancellation APIs and cancellation-race machinery.
- Backend drain orchestration, autoscaling demand signals, and provider-specific provisioning.
- Telchar-owned binary-cache lookup, storage, publication, credentials, or durable publication state. Existing Nix, Attic, S3, post-build hooks, and `nix copy` own cache behavior.
- Redis-backed live logs, Telchar-native object-storage upload, or historical log replay. A bounded local zstd spool and external uploader remain post-MVP extension options.
- Active/passive or active/active high availability and distributed scheduler ownership.
- Hostile multi-tenant isolation, per-tenant stores, or per-path client authorization.
- Fixed-output derivation execution. A post-MVP implementation must admit and preserve supported output hash metadata, include it in shared-build identity, forward it unchanged through every backend, validate realized outputs through Nix, and prove correct-hash and wrong-hash behavior with stock-Nix fixtures.
- Reproducible-build consensus or cryptographic provenance for classic input-addressed outputs.
- Kubernetes, cloud batch, or additional backend kinds.
- OCI images, extended compatibility candidates, soak tests, and performance architecture changes unless required by an actual deployment.
- Supporting a durable database other than PostgreSQL.
- Extracting `nix-worker-protocol` into a separate repository before it has a real second consumer and stable independent API.
- Interactive shell access.

## Ralph start guidance

Do not start one loop for the entire file. Start at the first unchecked task whose dependencies are complete. Copy that task and its gate context into `.ralph/<task-id>-<slug>.md`. Suggested loop settings:

```text
itemsPerIteration = 1
reflectEvery = 5
maxIterations = 20
```

Decision/prototype tasks may require fewer iterations. VM, Nomad, load, or soak tasks may require more, but their task scope must not be widened. If a task exposes an unrecorded architectural choice, stop it, add a decision task to this plan, and block dependent work rather than deciding incidentally.
