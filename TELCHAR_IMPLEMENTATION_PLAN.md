# Telchar Implementation Plan

**Status:** In progress

This plan decomposes Telchar into Ralph-compatible tasks. Each task is intended to produce one small, reviewable behavior or one recorded architecture decision. Work must follow task order and gate dependencies.

## Ralph execution contract

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

- [ ] T030 Add parser fuzz target
  - Depends on: T029
  - Outcome: fuzz target covers primitive framing and has documented bounded smoke command.
  - Red: target absent from fuzz manifest.
  - Verify: short deterministic fuzz smoke run.
  - Evidence: command and no-crash summary.

- [ ] T031 Parse client worker magic
  - Depends on: T028
  - Outcome: server accepts exact pinned client magic and rejects others.
  - Red: valid/invalid magic tests fail.
  - Verify: handshake magic tests.
  - Evidence: accepted and rejected values.

- [ ] T032 Emit server worker magic
  - Depends on: T031
  - Outcome: server writes exact response expected by pinned Nix.
  - Red: golden handshake output differs.
  - Verify: handshake golden test.
  - Evidence: bytes or fixture hash.

- [ ] T033 Negotiate supported worker version
  - Depends on: T032
  - Outcome: server selects version within initial matrix and records negotiated features.
  - Red: version table tests fail.
  - Verify: version negotiation tests.
  - Evidence: accepted boundaries.

- [ ] T034 Reject unsupported worker version
  - Depends on: T033
  - Outcome: unsupported old/new versions fail deterministically.
  - Red: server continues after mismatch.
  - Verify: version rejection tests.
  - Evidence: errors for both boundaries.

- [ ] T035 Complete pinned-client stdio handshake
  - Depends on: T033
  - Outcome: real pinned Nix completes handshake against `telchar serve-stdio`.
  - Red: real-client integration test fails at handshake.
  - Verify: direct stdio integration command.
  - Evidence: client version, negotiated protocol, clean server exit.

- [ ] T036 Parse worker operation code
  - Depends on: T035
  - Outcome: reusable protocol code parses operation codes using primary Nix constants without assuming request boundaries from raw byte chunks.
  - Red: operation-code fixture is unrecognized.
  - Verify: operation-code parser unit test.
  - Evidence: tested operation codes and primary source references.

- [ ] T036A Inventory typed fixture-flow requests
  - Depends on: T011, T019, T036
  - Outcome: versioned manifest maps every request, response, callback, and upload flow reachable by the compatibility fixtures to exact primary Nix serializers and bounded protocol types.
  - Red: fixture-flow inventory reports an unknown or unbounded message shape.
  - Verify: observer coverage manifest validator.
  - Evidence: operation/message list, protocol-version conditions, primary source references, and explicit unsupported flows.

- [ ] T036B Parse typed fixture-flow messages
  - Depends on: T025, T028, T036A
  - Outcome: `nix-worker-protocol` can parse and relay every inventoried fixture-flow message with exact operation boundaries while retaining no secret or payload body in trace records.
  - Red: golden fixtures fail before typed message parsers exist.
  - Verify: typed observer parser golden tests.
  - Evidence: per-message fixtures, bounds, and retained metadata fields.

- [ ] T036C Relay bounded uploads and callbacks transparently
  - Depends on: T029, T036B
  - Outcome: observer streams inventoried uploads, responses, and callbacks bidirectionally without whole-payload buffering, records only approved bounded metadata, and fails closed on an untyped flow.
  - Red: streaming fixture buffers beyond its bound, loses bytes, or accepts an untyped message.
  - Verify: transparent relay integration tests with large upload, callback, and unknown-flow cases.
  - Evidence: byte-for-byte relay hashes, observed memory bound, rejected flow, and sanitized telemetry.

### Compatibility traces and protocol evidence

- [ ] T012 Add worker-protocol trace capture fixture
  - Depends on: T011, T036C
  - Outcome: a transparent typed test peer relays pinned stock-Nix traffic while capturing operation codes and bounded protocol metadata without storing secrets or payload bodies.
  - Red: real-client trace assertion fails before the typed observer is wired.
  - Verify: real-client transparent trace command.
  - Evidence: sanitized trace artifact and proof that every observed request used a typed boundary parser.

- [ ] T013 Capture trusted classic derivation trace
  - Depends on: T012
  - Outcome: record handshake and operation sequence for trusted classic input-addressed remote build.
  - Red: compatibility matrix cell lacks evidence.
  - Verify: rerun trace fixture for cell.
  - Evidence: protocol version, trust result, operation sequence.

- [ ] T014 Capture untrusted classic derivation trace
  - Depends on: T012
  - Outcome: record whether pinned Nix uses `BuildDerivation`, `BuildPathsWithResults`, or another operation.
  - Red: compatibility matrix cell lacks evidence.
  - Verify: rerun untrusted trace fixture.
  - Evidence: operation sequence and trust negotiation.

- [ ] T015 Capture content-addressed derivation trace
  - Depends on: T012
  - Outcome: record operation and result semantics for one supported content-addressed build or explicitly defer it.
  - Red: compatibility matrix CA cell unresolved.
  - Verify: rerun CA fixture or matrix deferral validation.
  - Evidence: trace or explicit unsupported decision.

- [ ] T016 Define initial worker-operation allowlist
  - Depends on: T013, T014, T015
  - Outcome: document required, optional, recognized-rejected, and unknown operation behavior.
  - Red: captured trace contains unclassified operation.
  - Verify: classifier script over trace artifacts.
  - Evidence: zero unclassified operations.

- [ ] T020 Inventory independent protocol behaviors
  - Depends on: T016, T018, T019
  - Outcome: every required behavior maps to captured traffic, primary Nix source/documentation references, and an independent implementation/test task; Rio contributes only architecture or test-category notes.
  - Red: required behavior depends on Rio implementation details or lacks primary evidence.
  - Verify: protocol evidence inventory cross-check script.
  - Evidence: per-behavior evidence sources and task mapping.

### Gate 0 acceptance

- [ ] T021 Verify Gate 0 from clean checkout
  - Depends on: T008, T009, T009E, T010, T016, T018, T020
  - Outcome: clean checkout enters dev shell, reports pinned versions, passes baseline checks, exports correlated OTLP smoke signals, and validates compatibility records and provenance.
  - Red: gate script reports any missing artifact.
  - Verify: `nix flake check` plus repository gate script.
  - Evidence: exact commands and clean output summary.

### Reusable NixOS integration harness

- [ ] T021A Define reusable `nixosTest` topology contract
  - Depends on: T021
  - Outcome: ADR defines authoritative multi-machine integration topology, machine roles, shared helpers, service readiness, test artifacts, secrets handling, and when specialized tests extend rather than duplicate the harness.
  - Red: integration inventory finds an external boundary with no machine role, readiness rule, or artifact policy.
  - Verify: NixOS test-topology contract check.
  - Evidence: topology diagram, extension points, and mapped future integration tasks.

- [ ] T021B Add reusable `nixosTest` library
  - Depends on: T021A
  - Outcome: flake exports shared NixOS test modules/helpers for Telchar packaging, stock-Nix clients, networking, OpenSSH, OTLP collection, machine startup, and failure artifact capture.
  - Red: minimal test cannot instantiate two machines through shared helpers.
  - Verify: evaluate minimal multi-machine `nixosTest`.
  - Evidence: exported test attribute, machine definitions, and evaluation result.

- [ ] T021C Add baseline client-gateway integration smoke test
  - Depends on: T021B
  - Outcome: authoritative `nixosTest` boots separate client and gateway machines, runs the packaged Telchar service, reaches it over the declared network boundary, and captures correlated OTLP startup telemetry.
  - Red: smoke test fails before service, networking, readiness, and collector wiring are complete.
  - Verify: flake NixOS smoke-test command.
  - Evidence: machine topology, service readiness, network assertion, and correlated telemetry artifact.

- [ ] T021D Preserve deterministic NixOS test failure artifacts
  - Depends on: T021C
  - Outcome: failed integration tests retain bounded service journals, machine state, OTLP records, and driver output while successful tests clean temporary state and emit pristine output.
  - Red: controlled failure loses diagnostics, leaks secrets, or leaves unmanaged state.
  - Verify: controlled-failure artifact and cleanup test.
  - Evidence: artifact paths, redaction assertions, and cleanup proof.

- [ ] T021E Wire NixOS smoke test into repository gates
  - Depends on: T021C, T021D
  - Outcome: flake checks expose a rerunnable authoritative integration target, and future real-component fixtures extend the shared `nixosTest` harness instead of creating parallel orchestration systems.
  - Red: aggregate validation omits the smoke test or fixture policy permits duplicate harnesses.
  - Verify: `nix flake check` plus direct NixOS smoke-test command.
  - Evidence: flake attributes, aggregate output, direct command, and runtime summary.

## Gate 1 — Stdio worker-protocol proof

### Post-capture dispatch safety

- [ ] T037 Reject unknown operation code
  - Depends on: T016, T021E, T036
  - Outcome: unknown code produces deterministic Nix-compatible error framing.
  - Red: client sees EOF or panic.
  - Verify: unknown-operation integration test.
  - Evidence: captured asserted client error.

- [ ] T038 Reject recognized unsupported operation
  - Depends on: T016, T037
  - Outcome: allowlist rejects a known deferred operation distinctly from unknown input.
  - Red: unsupported operation is dispatched or reported as unknown.
  - Verify: unsupported-operation test.
  - Evidence: operation and asserted error class.

- [ ] T039 Bound per-session protocol allocations
  - Depends on: T026, T036
  - Outcome: session cumulative allocation budget rejects excess input.
  - Red: sequence exceeding budget succeeds.
  - Verify: session-budget test.
  - Evidence: budget and rejection point.

- [ ] T040 Bound protocol session idle time
  - Depends on: T035
  - Outcome: stalled partial request ends with configured timeout and clean resources.
  - Red: integration test hangs or leaks process.
  - Verify: timeout test with bounded wall clock.
  - Evidence: duration and cleanup assertion.

### Independent protocol behavior

- [ ] T041 Implement first inventoried protocol behavior independently
  - Depends on: T020, T035
  - Outcome: implement one required behavior in `nix-worker-protocol` from captured traffic, primary Nix source/documentation, and a failing compatibility or behavior test without copying or translating Rio source.
  - Red: named compatibility or behavior test fails before implementation.
  - Verify: crate behavior tests, real compatibility test, and evidence inventory validation.
  - Evidence: primary evidence references, test result, and no-copy attestation.

- [ ] T042 Record Rio-informed edge-case and test inventory
  - Depends on: T017, T018, T041
  - Outcome: compare Rio's architecture and test categories against current crate coverage, adding missing test ideas without copying implementation or test bodies.
  - Red: reference review identifies an untracked edge-case category.
  - Verify: reference-to-test-category checklist.
  - Evidence: categories adopted, deferred, or rejected with reasons.

- [ ] T043 Implement structured error framing independently
  - Depends on: T020, T037
  - Outcome: `nix-worker-protocol` emits error and activity frames required by the pinned client using captured traffic and primary Nix references without copying or translating Rio source.
  - Red: real client reports undecodable EOF/error.
  - Verify: crate framing tests and real-client expected-error test.
  - Evidence: primary evidence references and captured clean client message.

- [ ] T044 Bound structured log and error frame sizes
  - Depends on: T043
  - Outcome: oversized outbound/inbound log metadata is rejected or truncated by explicit policy.
  - Red: frame exceeds configured budget.
  - Verify: frame-bound tests.
  - Evidence: bounds and asserted behavior.

### Gate 1 acceptance

- [ ] T045 Verify Gate 1 stdio protocol proof
  - Depends on: T030, T035, T038, T039, T040, T041, T042, T043, T044
  - Outcome: real pinned Nix negotiates over stdio; malformed, oversized, unsupported, and unknown inputs fail cleanly.
  - Red: gate script reports missing evidence.
  - Verify: protocol unit/property/fuzz-smoke and real-client stdio suite.
  - Evidence: exact commands and pristine output.

## Gate 2 — Restricted OpenSSH ingress

### Ingress decision and identity handoff

- [ ] T046 Document OpenSSH process and IPC threat model
  - Depends on: T045
  - Outcome: ADR defines frontend/daemon privilege boundary, trusted metadata sources, local peer authentication, and spoofing threats.
  - Red: threat checklist exposes unspecified trust edge.
  - Verify: ADR checklist.
  - Evidence: data-flow diagram and mitigations.

- [ ] T047 Prototype public-key identity handoff
  - Depends on: T046
  - Outcome: forced command receives an authenticated key identity through OpenSSH-controlled configuration or records the approach as infeasible.
  - Red: spoofing fixture can replace identity metadata.
  - Verify: real OpenSSH key-auth fixture.
  - Evidence: key fingerprint and spoof rejection.

- [ ] T048 Prototype certificate identity handoff
  - Depends on: T047
  - Outcome: capture CA, key ID, and principals securely or explicitly defer certificate support.
  - Red: matrix marks certificate support unresolved.
  - Verify: real OpenSSH certificate fixture or deferral validation.
  - Evidence: authenticated metadata or recorded deferral.

- [ ] T048A Approve supported authenticated identity path
  - Depends on: T047, T048
  - Outcome: ADR identifies at least one proven OpenSSH-controlled identity path for initial ingress; if none exists, block Gate 2 and add a separately reviewed ingress redesign task.
  - Red: no supported path has spoof-resistant evidence.
  - Verify: identity evidence checklist and negative spoof test.
  - Evidence: approved mechanism and deferred mechanisms, or explicit blocker.

- [ ] T049 Define requester normalization
  - Depends on: T048A
  - Outcome: credential ID, audit subject, quota subject, certificate metadata, and source address normalize deterministically.
  - Red: table-driven normalization tests fail.
  - Verify: identity unit tests.
  - Evidence: public-key and certificate cases.

### Frontend and local IPC

- [ ] T050 Define local IPC message envelope
  - Depends on: T046
  - Outcome: versioned envelope carries trusted requester metadata, session ID, stream attachment, and bounded error data.
  - Red: schema round-trip tests fail.
  - Verify: IPC schema tests.
  - Evidence: supported version and size bounds.

- [ ] T051 Authenticate local frontend peer
  - Depends on: T050
  - Outcome: daemon accepts only expected local OS identity or socket credentials.
  - Red: wrong-user fixture connects successfully.
  - Verify: local socket authorization test.
  - Evidence: allowed and denied peer facts.

- [ ] T052 Connect `serve-stdio` frontend to daemon
  - Depends on: T051
  - Outcome: frontend forwards one protocol stream to daemon without owning scheduler or database state.
  - Red: end-to-end local IPC handshake fails.
  - Verify: frontend-daemon handshake test.
  - Evidence: process IDs and successful negotiation.

- [ ] T053 Bound frontend buffering
  - Depends on: T052
  - Outcome: slow daemon or client cannot cause unbounded frontend memory.
  - Red: backpressure test exceeds configured buffer.
  - Verify: bounded-stream test.
  - Evidence: buffer limits and observed maximum.

### SSH restrictions

- [ ] T054 Generate isolated OpenSSH fixture
  - Depends on: T048A, T052
  - Outcome: NixOS or isolated sshd fixture generates host/client keys and forced-command configuration reproducibly.
  - Red: fixture boot/connect test fails.
  - Verify: fixture start/connect/cleanup command.
  - Evidence: ports, generated paths, cleanup.

- [ ] T055 Complete worker handshake through `ssh-ng://`
  - Depends on: T054
  - Outcome: pinned stock Nix completes supported handshake through real OpenSSH.
  - Red: integration test fails at transport or protocol boundary.
  - Verify: `ssh-ng://` handshake test.
  - Evidence: client URI, negotiated protocol, request identity.

- [ ] T056 Reject arbitrary SSH command
  - Depends on: T054
  - Outcome: requested shell command is replaced by Telchar forced command.
  - Red: arbitrary command executes.
  - Verify: negative SSH command test.
  - Evidence: asserted denial.

- [ ] T057 Reject SSH PTY
  - Depends on: T054
  - Outcome: PTY allocation fails.
  - Red: PTY succeeds.
  - Verify: negative PTY test.
  - Evidence: asserted OpenSSH result.

- [ ] T058 Reject SSH TCP forwarding
  - Depends on: T054
  - Outcome: local, remote, and dynamic forwarding are disabled.
  - Red: forwarding listener or connection succeeds.
  - Verify: forwarding negative tests.
  - Evidence: all denied modes.

- [ ] T059 Reject SSH agent and X11 forwarding
  - Depends on: T054
  - Outcome: agent and X11 forwarding are unavailable.
  - Red: forwarded socket/display appears.
  - Verify: forwarding environment negative tests.
  - Evidence: absence assertions.

- [ ] T060 Ignore client-supplied identity environment
  - Depends on: T049, T054
  - Outcome: spoofed environment cannot alter normalized requester.
  - Red: spoof fixture changes requester.
  - Verify: identity spoof integration test.
  - Evidence: trusted requester remains unchanged.

### Gate 2 acceptance

- [ ] T061 Verify Gate 2 restricted ingress
  - Depends on: T048A, T055, T056, T057, T058, T059, T060
  - Outcome: real stock Nix reaches Telchar through `ssh-ng://`; identity is trustworthy; prohibited SSH features fail.
  - Red: gate script reports missing negative test.
  - Verify: complete OpenSSH integration suite.
  - Evidence: exact command and pristine output.

## Gate 3 — Gateway store and local vertical slice

### Store boundary

- [ ] T062 Document dedicated gateway-store ownership
  - Depends on: T061
  - Outcome: ADR specifies service account, daemon interaction, privileges, GC ownership, and no unrelated host workloads.
  - Red: privilege checklist exposes unspecified operation.
  - Verify: ADR checklist.
  - Evidence: required permissions and trust boundary.

- [ ] T063 Create real Nix store test fixture
  - Depends on: T062
  - Outcome: reproducible fixture provisions known store state and cleans it safely.
  - Red: fixture leaves path/database/process residue.
  - Verify: setup/build/teardown self-test.
  - Evidence: pre/post state.

- [ ] T064 Query path validity
  - Depends on: T063
  - Outcome: store adapter reports one valid and one invalid path through real Nix.
  - Red: integration test cannot distinguish paths.
  - Verify: real-store validity test.
  - Evidence: tested store paths.

- [ ] T065 Query path metadata
  - Depends on: T064
  - Outcome: adapter returns NAR hash, size, references, deriver, and content-address metadata required by protocol target.
  - Red: metadata test lacks expected fields.
  - Verify: real-store metadata test.
  - Evidence: asserted fields.

- [ ] T066 Import one NAR with path metadata
  - Depends on: T065
  - Outcome: real store accepts a valid NAR and registers expected path info.
  - Red: import integration test fails before adapter implementation.
  - Verify: real-store import test.
  - Evidence: imported path and metadata.

- [ ] T067 Reject corrupt NAR import
  - Depends on: T066
  - Outcome: hash/content mismatch fails without valid path registration.
  - Red: corrupt import appears valid.
  - Verify: corrupt-NAR test.
  - Evidence: asserted failure and invalid path.

- [ ] T068 Export one valid path as NAR
  - Depends on: T066
  - Outcome: adapter streams valid path and metadata back to caller.
  - Red: exported NAR differs from store content.
  - Verify: round-trip NAR test.
  - Evidence: content/hash equality.

- [ ] T069 Bound NAR transfer bytes
  - Depends on: T066, T068
  - Outcome: inbound and outbound transfers obey configured per-object and per-session limits.
  - Red: over-limit transfer succeeds.
  - Verify: transfer-limit tests.
  - Evidence: limits and rejection points.

- [ ] T069A Bound transferred object counts
  - Depends on: T066, T068
  - Outcome: per-session and global object-count budgets reject excess uploads/downloads before registration or unbounded bookkeeping.
  - Red: sequence above configured count succeeds.
  - Verify: object-count admission tests.
  - Evidence: limits and rejection point.

- [ ] T069B Enforce transfer rate policy
  - Depends on: T066, T068
  - Outcome: configured transfer-rate policy throttles or rejects sustained excess traffic without unbounded buffering.
  - Red: controlled sender exceeds policy without throttle/rejection.
  - Verify: time-bounded transfer-rate integration test.
  - Evidence: configured rate and observed behavior.

- [ ] T070 Enforce gateway disk reserve
  - Depends on: T063
  - Outcome: new transfer/build admission fails before configured free-space reserve is crossed.
  - Red: low-space fixture admits work.
  - Verify: disk-reserve test using controlled filesystem fixture.
  - Evidence: reserve and asserted rejection.

### Minimum durable request and lease state

- [ ] T070A Add minimum PostgreSQL migration runner
  - Depends on: T063
  - Outcome: Gate 3 daemon applies ordered PostgreSQL migrations for sessions, requests, attachments, and store leases transactionally.
  - Red: empty PostgreSQL database lacks minimum lifecycle schema.
  - Verify: real PostgreSQL migration integration test.
  - Evidence: PostgreSQL version, schema version, and tables.

- [ ] T070B Persist minimum protocol session
  - Depends on: T070A
  - Outcome: domain-specific session state operation persists session ID, requester reference, and open/closed state across process restart.
  - Red: session round-trip/restart test fails.
  - Verify: real PostgreSQL session state-operation test.
  - Evidence: persisted fields and transaction boundary.

- [ ] T070C Persist minimum build request
  - Depends on: T070B
  - Outcome: accepted build has durable immutable request identity before leases or execution.
  - Red: accepted request disappears after restart.
  - Verify: request persistence test.
  - Evidence: request row and identifier.

- [ ] T070D Persist minimum request attachment
  - Depends on: T070C
  - Outcome: protocol session attachment is durable and distinct from request state.
  - Red: detach/restart test conflates session and request.
  - Verify: attachment persistence test.
  - Evidence: attached/detached states.

### GC leases

- [ ] T071 Define store lease record
  - Depends on: T070C
  - Outcome: durable PostgreSQL lease identifies request/publication owner, path, purpose, and release state behind domain-specific lease operations.
  - Red: schema and operation tests fail.
  - Verify: real PostgreSQL migration and lease-operation tests.
  - Evidence: fields, constraints, and transaction ownership.

- [ ] T072 Acquire derivation lease on accepted build
  - Depends on: T071
  - Outcome: accepted request roots derivation before queue visibility.
  - Red: state transition lacks root.
  - Verify: transaction integration test.
  - Evidence: request and root records.

- [ ] T073 Acquire complete input-closure leases
  - Depends on: T072
  - Outcome: accepted request roots every required input path.
  - Red: closure fixture contains unleased path.
  - Verify: closure lease test.
  - Evidence: exact closure set.

- [ ] T074 Preserve leased paths across GC
  - Depends on: T073
  - Outcome: real GC cannot remove queued/running request inputs.
  - Red: GC removes fixture path.
  - Verify: real-store GC test.
  - Evidence: path valid before/after GC.

- [ ] T075 Release request leases transactionally
  - Depends on: T074
  - Outcome: terminal cleanup releases only eligible request roots after delivery/detachment policy.
  - Red: early release or leaked lease test fails.
  - Verify: lifecycle lease tests.
  - Evidence: state and root transitions.

### Build operation and local backend

- [ ] T076 Parse supported derivation build operation
  - Depends on: T016, T065
  - Outcome: gateway normalizes one captured build operation into `BuildRequest` without backend objects.
  - Red: captured fixture fails to parse.
  - Verify: operation fixture test.
  - Evidence: normalized fields.

- [ ] T077 Reject unsupported build option
  - Depends on: T076
  - Outcome: unsafe or unsupported client option fails deterministically.
  - Red: option passes through silently.
  - Verify: build-option allowlist test.
  - Evidence: rejected option and error.

- [ ] T078 Normalize supported build options
  - Depends on: T077
  - Outcome: allowed options map to explicit internal values and defaults.
  - Red: table-driven option tests fail.
  - Verify: option normalization tests.
  - Evidence: supported set.

- [ ] T079 Define local execution request
  - Depends on: T076, T078
  - Outcome: local executor receives derivation, system/features, allowed options, closure references, request ID, and cancellation token.
  - Red: schema tests fail.
  - Verify: request schema tests.
  - Evidence: required fields.

- [ ] T080 Execute one derivation in gateway store
  - Depends on: T079
  - Outcome: local backend realizes one derivation through structured process arguments or Nix API, without shell interpolation.
  - Red: real local execution test fails.
  - Verify: real-store local build test.
  - Evidence: derivation and exit/result data.

- [ ] T081 Capture local build log
  - Depends on: T080
  - Outcome: build log is streamed into bounded internal log channel.
  - Red: integration test cannot observe expected builder line.
  - Verify: real-build log test.
  - Evidence: asserted line and buffer bound.

- [ ] T082 Apply log backpressure
  - Depends on: T081
  - Outcome: slow protocol attachment cannot grow memory beyond configured buffer.
  - Red: slow-reader test exceeds bound.
  - Verify: bounded log streaming test.
  - Evidence: observed maximum and policy.

- [ ] T083 Map successful local result
  - Depends on: T080
  - Outcome: supported Nix result fields map to normalized outcome.
  - Red: result fixture lacks required fields.
  - Verify: success mapping test.
  - Evidence: mapped fields.

- [ ] T084 Reject zero exit with missing expected output
  - Depends on: T083
  - Outcome: process success cannot produce Telchar success when expected output is absent.
  - Red: fault fixture reports success.
  - Verify: missing-output integration test.
  - Evidence: asserted output failure.

- [ ] T085 Reject invalid imported output metadata
  - Depends on: T067, T083
  - Outcome: mismatched NAR/path metadata produces output failure.
  - Red: invalid output accepted.
  - Verify: invalid-output test.
  - Evidence: asserted validation error.

- [ ] T085A Acquire request output leases before success
  - Depends on: T075, T083, T085
  - Outcome: every verified output is rooted atomically before result becomes deliverable; rollback removes partial multi-output leases.
  - Red: success transaction can expose unrooted output or partial lease set.
  - Verify: real-store multi-output lease and rollback tests.
  - Evidence: output roots and atomic failure case.

- [ ] T086 Preserve classic output trust statement in outcome
  - Depends on: T083
  - Outcome: code and docs distinguish store validation from provenance proof for input-addressed outputs.
  - Red: result documentation test finds overclaim.
  - Verify: outcome/docs consistency test.
  - Evidence: assertion location.

- [ ] T086A Expand plan for every required worker operation
  - Depends on: T016, T064, T065, T066, T068, T076
  - Outcome: for every operation classified required by T016, add or identify a focused decoder, dispatcher, store behavior, response-framing task, and real-client test before end-to-end success.
  - Red: operation coverage checker finds required operation without complete implementation/test mapping.
  - Verify: operation coverage script against allowlist and plan manifest.
  - Evidence: zero uncovered required operations and added task IDs.

- [ ] T087 Return successful build result over stdio
  - Depends on: T083, T084, T085, T085A, T086A
  - Outcome: pinned Nix client receives successful result and can copy expected output.
  - Red: real-client vertical test fails after build.
  - Verify: direct-stdio end-to-end build.
  - Evidence: client-visible output path/content.

- [ ] T088 Return successful build result over `ssh-ng://`
  - Depends on: T087, T061
  - Outcome: stock Nix client completes same build through restricted OpenSSH.
  - Red: SSH vertical test fails.
  - Verify: `ssh-ng://` end-to-end build.
  - Evidence: request ID and client-visible output.

- [ ] T089 Prove client cannot build acceptance derivation locally
  - Depends on: T088
  - Outcome: primary fixture ensures success came from Telchar backend, not local fallback.
  - Red: fixture unexpectedly builds with remote disabled.
  - Verify: negative-local then positive-remote test.
  - Evidence: local failure and remote success.

- [ ] T090 Define disconnect policy by lifecycle point
  - Depends on: T088
  - Outcome: ADR covers upload, queued, running, collecting, and result-delivery disconnects; first-release reattachment status explicit.
  - Red: lifecycle table has unspecified cell.
  - Verify: policy table validator.
  - Evidence: all cells resolved.

- [ ] T091 Cancel incomplete upload on disconnect
  - Depends on: T090
  - Outcome: partial upload is discarded and resources released.
  - Red: disconnect fixture leaves valid path or retained bytes.
  - Verify: upload disconnect test.
  - Evidence: cleanup assertions.

- [ ] T092 Detach running request without cancelling execution
  - Depends on: T090
  - Outcome: transport loss marks attachment detached while local execution continues.
  - Red: running build is killed or request corrupted.
  - Verify: running disconnect integration test.
  - Evidence: detached state and eventual output.

- [ ] T093 Retain output after detached completion
  - Depends on: T092, T075, T085A
  - Outcome: output lease follows documented detached retention policy.
  - Red: GC removes output too early or lease never releases.
  - Verify: detached retention/cleanup test.
  - Evidence: timed/state-based lease transitions.

- [ ] T094 Extend NixOS vertical integration fixture
  - Depends on: T021E, T088, T089
  - Outcome: shared `nixosTest` harness provisions stock client, OpenSSH ingress, daemon, gateway store, and local executor as a reproducible end-to-end topology.
  - Red: vertical NixOS test fails before the shared fixture extension is complete.
  - Verify: flake NixOS vertical-test command.
  - Evidence: reused harness modules, VM topology, and output proof.

### Gate 3 acceptance

- [ ] T095 Verify Gate 3 local correctness vertical slice
  - Depends on: T069, T069A, T069B, T070, T070D, T074, T084, T085, T085A, T086A, T089, T091, T092, T093, T094
  - Outcome: stock client that cannot build locally receives verified output through Telchar; bounds, GC, invalid output, and disconnect policies pass.
  - Red: gate script reports missing evidence.
  - Verify: full protocol/store/OpenSSH/local-backend VM suite.
  - Evidence: exact command and pristine output.

## Gate 4 — Durable state, admission, and deterministic scheduling

### Durable request model

- [ ] T096 Extend PostgreSQL migrations for execution state
  - Depends on: T095, T070A
  - Outcome: ordered PostgreSQL migrations add attempts, outcomes, queue state, capacity reservations, and audit fields transactionally.
  - Red: Gate 3 database cannot represent execution lifecycle.
  - Verify: real PostgreSQL upgrade migration test from Gate 3 schema.
  - Evidence: PostgreSQL version, old/new schema versions, and preserved rows.

- [ ] T097 Harden protocol session state operation
  - Depends on: T096, T070B
  - Outcome: session state operation adds bounded audit metadata, timestamps, and constraints without exposing SQL or losing Gate 3 rows.
  - Red: migration/operation round-trip test fails.
  - Verify: real PostgreSQL session-operation upgrade test.
  - Evidence: preserved and added fields plus transaction boundary.

- [ ] T098 Harden build request state operation
  - Depends on: T097, T070C
  - Outcome: request state operation adds normalized immutable fields, queue metadata, and constraints without changing identity or exposing generic CRUD.
  - Red: migration/operation round-trip test fails.
  - Verify: real PostgreSQL request-operation upgrade test.
  - Evidence: preserved ID, added fields, and transaction boundary.

- [ ] T099 Harden request attachment state operation
  - Depends on: T098, T070D
  - Outcome: attachment state operation adds completed-delivery state, timestamps, and restart constraints.
  - Red: migration/operation round-trip test fails.
  - Verify: real PostgreSQL attachment-operation upgrade test.
  - Evidence: multiple attachment cases and transaction boundary.

- [ ] T100 Persist execution attempt
  - Depends on: T098
  - Outcome: attempt has stable ID, ordinal, idempotency key, backend, and state.
  - Red: attempt schema test fails.
  - Verify: attempt persistence test.
  - Evidence: uniqueness constraints.

- [ ] T101 Persist immutable terminal outcome
  - Depends on: T100
  - Outcome: terminal outcome and classification cannot be overwritten.
  - Red: update-after-terminal succeeds.
  - Verify: immutability test.
  - Evidence: rejected mutation.

### State transitions and recovery

- [ ] T102 Transition accepted request to queued
  - Depends on: T098
  - Outcome: transaction creates queue state only after request and leases are durable.
  - Red: partial transaction fixture exposes queued request without leases.
  - Verify: transaction fault test.
  - Evidence: rollback and success cases.

- [ ] T103 Transition queued request to dispatching
  - Depends on: T100, T102
  - Outcome: atomic transaction creates attempt and reserves capacity before backend submission.
  - Red: concurrent dispatch creates duplicate attempts or exceeds capacity.
  - Verify: concurrent real PostgreSQL transition test using row locking and transactional capacity reservation.
  - Evidence: single active attempt.

- [ ] T104 Transition dispatching attempt to backend-pending
  - Depends on: T103
  - Outcome: backend execution ID persists with attempt.
  - Red: state/ID partial write appears.
  - Verify: transition transaction test.
  - Evidence: atomic row state.

- [ ] T105 Transition pending attempt to running
  - Depends on: T104
  - Outcome: running timestamp and counters update atomically.
  - Red: counter/state mismatch test fails.
  - Verify: transition test.
  - Evidence: state and counters.

- [ ] T106 Transition running attempt to collecting
  - Depends on: T105
  - Outcome: execution completion and output collection are distinct.
  - Red: process exit directly marks request successful.
  - Verify: transition test.
  - Evidence: collecting state.

- [ ] T107 Transition collecting attempt to terminal success
  - Depends on: T106
  - Outcome: verified output and result metadata atomically complete attempt/request.
  - Red: success without output lease/metadata succeeds.
  - Verify: success transaction test.
  - Evidence: all terminal records.

- [ ] T108 Transition attempt to terminal failure
  - Depends on: T103
  - Outcome: failure classification and timing persist immutably.
  - Red: missing classification accepted.
  - Verify: failure transition tests.
  - Evidence: supported classifications.

- [ ] T109 Recover queued requests after daemon restart
  - Depends on: T102
  - Outcome: queued work is reconstructed deterministically.
  - Red: restart loses or duplicates queue entries.
  - Verify: process restart integration test.
  - Evidence: before/after request IDs.

- [ ] T110 Recover dispatching attempt before backend ID persistence
  - Depends on: T103
  - Outcome: recovery marks attempt ambiguous and reconciles idempotency key before resubmission.
  - Red: restart blindly submits duplicate.
  - Verify: crash-point integration test.
  - Evidence: submission count and state.

- [ ] T111 Recover backend-pending attempt
  - Depends on: T104
  - Outcome: daemon reconciles known backend execution instead of creating another attempt.
  - Red: restart submits duplicate.
  - Verify: restart/reconciliation test with real local execution registry.
  - Evidence: one backend execution.

- [ ] T112 Recover running attempt
  - Depends on: T105
  - Outcome: daemon reconciles completion/running state and preserves logs/outcome rules.
  - Red: restart loses running attempt.
  - Verify: restart during long local build.
  - Evidence: eventual state and attempt count.

- [ ] T113 Recover collecting attempt
  - Depends on: T106
  - Outcome: output collection resumes idempotently.
  - Red: restart duplicates or loses import.
  - Verify: collection crash-point test.
  - Evidence: one valid output and terminal state.

### Identity and admission

- [ ] T114 Persist credential identity
  - Depends on: T049, T096
  - Outcome: credential ID and authentication authority are durable audit fields.
  - Red: round-trip test fails.
  - Verify: identity persistence test.
  - Evidence: key and certificate cases.

- [ ] T115 Map credential to audit subject
  - Depends on: T114
  - Outcome: configured mapping selects stable audit subject with explicit fallback.
  - Red: table-driven mapping test fails.
  - Verify: mapping tests.
  - Evidence: mapped/unmapped cases.

- [ ] T116 Map credential to quota subject
  - Depends on: T114
  - Outcome: multiple credentials can share one quota subject; fallback is credential ID.
  - Red: multiple-key quota test fails.
  - Verify: mapping tests.
  - Evidence: shared and fallback cases.

- [ ] T117 Enforce global concurrent session limit
  - Depends on: T097
  - Outcome: excess SSH protocol sessions receive clean admission error.
  - Red: concurrent fixture exceeds limit.
  - Verify: real concurrent SSH session test.
  - Evidence: accepted/rejected counts.

- [ ] T118 Enforce global retained-byte limit
  - Depends on: T069, T073
  - Outcome: aggregate retained inputs cannot exceed configured budget.
  - Red: concurrent uploads exceed budget.
  - Verify: retained-byte concurrency test.
  - Evidence: bytes and rejection.

- [ ] T119 Enforce per-quota-subject retained-byte limit
  - Depends on: T116, T118
  - Outcome: credentials mapped to same subject share transfer/storage budget.
  - Red: two-key fixture bypasses limit.
  - Verify: mapped-credential integration test.
  - Evidence: shared accounting.

- [ ] T120 Enforce global queued request limit
  - Depends on: T102
  - Outcome: excess build operation receives clean Nix-compatible admission error.
  - Red: queue exceeds configured maximum.
  - Verify: queue limit test.
  - Evidence: accepted/rejected request IDs.

- [ ] T121 Enforce per-quota-subject queued limit
  - Depends on: T116, T120
  - Outcome: mapped credentials share queued quota.
  - Red: second credential bypasses queue limit.
  - Verify: real multi-key request test.
  - Evidence: shared count.

- [ ] T122 Enforce global dispatching limit
  - Depends on: T103
  - Outcome: scheduler cannot reserve more concurrent submissions than configured.
  - Red: concurrent dispatcher exceeds limit.
  - Verify: dispatch concurrency test.
  - Evidence: maximum observed.

- [ ] T123 Enforce global backend-pending limit
  - Depends on: T104
  - Outcome: pending capacity is distinct and bounded.
  - Red: pending attempts exceed limit.
  - Verify: controlled pending backend test.
  - Evidence: maximum and queued remainder.

- [ ] T124 Enforce global running limit
  - Depends on: T105
  - Outcome: running attempts never exceed configured maximum.
  - Red: concurrent execution exceeds limit.
  - Verify: long-running real local builds.
  - Evidence: observed concurrency.

- [ ] T124A Enforce global collecting limit
  - Depends on: T106
  - Outcome: concurrent output collection never exceeds configured global capacity.
  - Red: controlled collectors exceed limit.
  - Verify: concurrent collection test.
  - Evidence: maximum observed and queued remainder.

- [ ] T125 Enforce per-quota-subject state limits
  - Depends on: T116, T122, T123, T124, T124A
  - Outcome: separate configured subject limits apply to queued, dispatching, backend-pending, running, and collecting states without multi-key bypass.
  - Red: mapped credentials exceed any state-specific limit.
  - Verify: concurrent mapped-credential tests for every state.
  - Evidence: per-state configured and observed counts.

### Deterministic scheduler

- [ ] T126 Order requests FIFO within one quota subject
  - Depends on: T102
  - Outcome: stable sequence uses durable enqueue order, not hash iteration or wall-clock ties.
  - Red: ordering table test fails.
  - Verify: scheduler unit test.
  - Evidence: selected order.

- [ ] T127 Round-robin runnable quota subjects
  - Depends on: T126
  - Outcome: scheduler rotates across subjects with runnable work.
  - Red: one subject monopolizes selections.
  - Verify: table-driven scheduler test.
  - Evidence: dispatch sequence.

- [ ] T128 Skip incompatible head without starving compatible work
  - Depends on: T127
  - Outcome: one subject's incompatible oldest request does not block its later compatible request according to explicit policy.
  - Red: scheduler stalls despite compatible work.
  - Verify: mixed-system table test.
  - Evidence: selected request and retained order.

- [ ] T129 Reject permanently unsupported capability combination
  - Depends on: T128
  - Outcome: request outside public capability envelope fails promptly.
  - Red: request queues forever.
  - Verify: unsupported-capability test.
  - Evidence: Nix-compatible error.

- [ ] T130 Keep temporarily unavailable compatible request queued
  - Depends on: T128
  - Outcome: request matching public envelope waits when eligible backend capacity is temporarily zero.
  - Red: request is rejected as unsupported.
  - Verify: capacity-state scheduler test.
  - Evidence: queued state and later dispatch.

- [ ] T131 Define and apply administrative priority classes
  - Depends on: T127
  - Outcome: ADR and tests define whether priority applies before or within identity round-robin, stable tie-breaking, and explicit starvation expectations; no aging policy is added without evidence.
  - Red: priority examples produce ambiguous selection or undocumented starvation.
  - Verify: ADR example table and scheduler tests.
  - Evidence: policy and dispatch sequences.

- [ ] T132 Select backend by system and required features
  - Depends on: T129
  - Outcome: compatibility filter admits only exact system/feature matches.
  - Red: incompatible backend selected.
  - Verify: capability matrix tests.
  - Evidence: eligible/ineligible sets.

- [ ] T133 Rank compatible backends deterministically
  - Depends on: T132
  - Outcome: administrative priority, configured preference/cost, active count, and stable name tie-break produce one result.
  - Red: equal candidates vary by insertion order.
  - Verify: permutation property test.
  - Evidence: stable selected backend.

- [ ] T134 Prove runnable subject non-starvation
  - Depends on: T127, T131
  - Outcome: property test shows every continuously runnable subject progresses under fixed capacity and documented assumptions.
  - Red: generated schedule finds starvation.
  - Verify: scheduler property suite.
  - Evidence: assumptions and case count.

### Retry and cancellation

- [ ] T135 Define failure transition matrix
  - Depends on: T108, T110, T111, T112, T113
  - Outcome: ADR maps each failure point to terminal/reconcile/retry behavior.
  - Red: matrix validator finds unspecified transition.
  - Verify: matrix completeness test.
  - Evidence: all state/failure cells.

- [ ] T136 Classify derivation build failure as terminal
  - Depends on: T135
  - Outcome: failed derivation creates no automatic retry.
  - Red: scheduler creates second attempt.
  - Verify: real failing derivation test.
  - Evidence: one attempt and terminal class.

- [ ] T137 Retry known pre-execution infrastructure failure
  - Depends on: T135
  - Outcome: explicitly safe failure creates one bounded linked attempt.
  - Red: no retry or unbounded retry occurs.
  - Verify: controlled infrastructure failure test.
  - Evidence: two linked attempts.

- [ ] T138 Refuse retry for ambiguous active attempt
  - Depends on: T135
  - Outcome: unknown backend activity enters reconciliation/blocked state, not duplicate submission.
  - Red: second attempt starts.
  - Verify: ambiguous submission test.
  - Evidence: single backend execution.

- [ ] T139 Enforce retry attempt bound
  - Depends on: T137
  - Outcome: attempts stop at configured maximum and become terminal.
  - Red: extra attempt appears.
  - Verify: retry-bound test.
  - Evidence: final count/classification.

- [ ] T140 Cancel queued request without attachments
  - Depends on: T099, T102
  - Outcome: last requester detachment cancels queued request and releases eligible leases.
  - Red: request remains queued.
  - Verify: detach/cancel lifecycle test.
  - Evidence: terminal state and roots.

- [ ] T141 Preserve running request after last detachment
  - Depends on: T099, T105
  - Outcome: running execution continues under conservative policy.
  - Red: detachment triggers backend cancel.
  - Verify: real long-build detach test.
  - Evidence: completion after detachment.

- [ ] T142 Administratively cancel running request
  - Depends on: T105
  - Outcome: admin action requests backend cancellation and records actor/reason.
  - Red: cancel action missing audit or has no effect.
  - Verify: real cancellable local build test.
  - Evidence: audit record and terminal result.

- [ ] T143 Resolve cancellation/completion race
  - Depends on: T142
  - Outcome: deterministic precedence retains valid output according to documented transition table.
  - Red: race produces mutable or contradictory terminal states.
  - Verify: repeated race integration test.
  - Evidence: allowed outcomes and immutability.

### Administration and observability

- [ ] T144 Add request status command
  - Depends on: T098, T100, T101
  - Outcome: CLI shows request, requester, attachments, attempts, backend, state, and timing without secrets.
  - Red: CLI golden test lacks fields.
  - Verify: CLI integration test against real PostgreSQL fixture.
  - Evidence: sanitized output.

- [ ] T145 Add queue listing command
  - Depends on: T126
  - Outcome: CLI lists durable queue order and runnable reason.
  - Red: ordering/reason golden test fails.
  - Verify: CLI queue test.
  - Evidence: output fixture.

- [ ] T146 Add administrative cancellation command
  - Depends on: T142
  - Outcome: CLI issues audited cancellation by request ID.
  - Red: command test fails.
  - Verify: CLI cancellation integration test.
  - Evidence: actor/reason/state.

- [ ] T147 Instrument request lifecycle telemetry
  - Depends on: T009E, T107, T108
  - Outcome: tracing spans and structured events cover session/request/attachment/attempt transitions with stable request/session/attempt/backend IDs, trace context, duration, result classification, and redacted bounded fields.
  - Red: OTLP log/trace assertion misses a lifecycle transition or correlation field.
  - Verify: lifecycle OTLP log and trace integration test.
  - Evidence: captured sanitized events, spans, and correlation values.

- [ ] T148 Export low-cardinality request metrics through OTLP
  - Depends on: T009E, T120, T124
  - Outcome: accepted/rejected/queued/dispatching/pending/running/collecting/completed/failed counts and durations export through OTLP with bounded attributes.
  - Red: collector test reports missing metric or raw identity/high-cardinality attribute.
  - Verify: OTLP metrics collector test.
  - Evidence: instrument names, units, and attributes.

- [ ] T149 Instrument backend and transfer telemetry
  - Depends on: T009E, T069, T132
  - Outcome: spans, events, and OTLP metrics cover transfer bytes/durations, backend selection, submission, state, cancellation, collection, and infrastructure failures with bounded attributes.
  - Red: collector test lacks a backend/transfer signal or exposes prohibited attribute.
  - Verify: backend/transfer OTLP logs, metrics, and traces integration test.
  - Evidence: signal names, spans, and bounded attributes.

- [ ] T150 Audit requester and administrative actions
  - Depends on: T114, T142, T146
  - Outcome: durable audit record links credential, audit/quota subject, source metadata, request, action, actor, and reason.
  - Red: audit completeness test fails.
  - Verify: real PostgreSQL audit-operation test.
  - Evidence: sanitized rows.

### Gate 4 acceptance

- [ ] T151 Verify Gate 4 durable scheduling
  - Depends on: T109, T110, T111, T112, T113, T119, T124A, T125, T129, T130, T133, T134, T136, T138, T139, T143, T150
  - Outcome: concurrent real sessions obey quotas and fairness; restart recovery avoids duplicate submission; audit and metrics remain coherent.
  - Red: gate script reports missing state or concurrency evidence.
  - Verify: durable-state, scheduler, concurrency, restart, CLI, metrics, and audit suites.
  - Evidence: exact commands and pristine output.

## Gate 5 — Remote execution contract

### Data-flow and authorization decision

- [ ] T152 Document remote executor data-flow threat model
  - Depends on: T151
  - Outcome: ADR selects push or pull transfer, trust boundaries, network paths, and authorization ownership.
  - Red: threat checklist exposes unspecified path.
  - Verify: ADR checklist.
  - Evidence: diagram and decision.

- [ ] T153 Define authorized input closure manifest
  - Depends on: T152
  - Outcome: immutable manifest names attempt, exact store paths, metadata, expiry, and nonce/version.
  - Red: schema tests fail.
  - Verify: manifest round-trip and validation tests.
  - Evidence: fields and bounds.

- [ ] T154 Define authorized output manifest
  - Depends on: T152
  - Outcome: manifest limits output names/paths and required metadata for one attempt.
  - Red: schema tests fail.
  - Verify: output manifest tests.
  - Evidence: fields and validation.

- [ ] T155 Issue request-scoped executor credential
  - Depends on: T153, T154
  - Outcome: credential is attempt-bound, time-bound, audience-bound, and revocable.
  - Red: credential validation tests fail.
  - Verify: issue/validate tests.
  - Evidence: claims and lifetime.

- [ ] T156 Deny unrelated input path
  - Depends on: T155
  - Outcome: executor credential cannot fetch store path outside authorized closure.
  - Red: negative authorization test succeeds.
  - Verify: real transfer service test.
  - Evidence: denied path and status.

- [ ] T157 Deny unauthorized output upload
  - Depends on: T155
  - Outcome: credential cannot upload undeclared output.
  - Red: negative upload succeeds.
  - Verify: real upload service test.
  - Evidence: denied output.

- [ ] T158 Expire executor credential
  - Depends on: T155
  - Outcome: expired credential cannot fetch or upload.
  - Red: post-expiry operation succeeds.
  - Verify: time-controlled credential test.
  - Evidence: expiry and denial.

- [ ] T159 Revoke executor credential
  - Depends on: T155
  - Outcome: cancellation/terminal state revokes future access.
  - Red: revoked credential remains usable.
  - Verify: revocation integration test.
  - Evidence: state and denial.

- [ ] T160 Prevent credential replay across attempts
  - Depends on: T155
  - Outcome: credential for one attempt fails against another attempt's resources.
  - Red: replay succeeds.
  - Verify: cross-attempt negative test.
  - Evidence: attempt IDs and denial.

### Transfer service

- [ ] T161 Serve authorized NAR metadata
  - Depends on: T153, T155
  - Outcome: executor can query metadata only for authorized paths.
  - Red: real request fails.
  - Verify: transfer service metadata test.
  - Evidence: authorized response.

- [ ] T162 Stream authorized input NAR
  - Depends on: T161
  - Outcome: executor downloads one authorized path with backpressure and byte accounting.
  - Red: end-to-end transfer fails.
  - Verify: real NAR download test.
  - Evidence: content/hash and metrics.

- [ ] T163 Resume or explicitly reject interrupted input transfer
  - Depends on: T162
  - Outcome: first-release behavior is deterministic and documented; partial state cleans safely.
  - Red: interrupted transfer leaves ambiguous state.
  - Verify: disconnect transfer test.
  - Evidence: retry/rejection and cleanup.

- [ ] T164 Upload authorized output NAR
  - Depends on: T154, T155
  - Outcome: executor uploads declared output into staging boundary.
  - Red: real upload fails.
  - Verify: output upload integration test.
  - Evidence: staged path and metadata.

- [ ] T165 Verify and import uploaded output
  - Depends on: T164, T085
  - Outcome: gateway validates NAR/path metadata and imports only valid declared output.
  - Red: valid upload not imported.
  - Verify: real-store collection test.
  - Evidence: valid store path.

- [ ] T166 Retry output collection idempotently
  - Depends on: T165
  - Outcome: repeated collection after transport failure yields one valid output and one outcome.
  - Red: duplicate/corrupt state appears.
  - Verify: interrupted collection test.
  - Evidence: import count and terminal state.

### Backend lifecycle contract

- [ ] T167 Define backend capability descriptor
  - Depends on: T132
  - Outcome: backend declares systems, features, dispatch/pending/running capacities, priority, and drain state.
  - Red: validation tests fail.
  - Verify: descriptor tests.
  - Evidence: valid/invalid cases.

- [ ] T168 Define idempotent backend submit contract
  - Depends on: T100, T153, T154
  - Outcome: submit uses attempt ID/idempotency key and returns stable execution identity.
  - Red: duplicate-submit contract test creates two executions.
  - Verify: local backend conformance test.
  - Evidence: one execution identity.

- [ ] T169 Define backend reconciliation contract
  - Depends on: T168
  - Outcome: backend reports absent/pending/running/completed/failed/cancelled/ambiguous with raw evidence.
  - Red: state mapping tests fail.
  - Verify: conformance tests.
  - Evidence: all states.

- [ ] T170 Define backend log cursor contract
  - Depends on: T169
  - Outcome: logs use bounded cursor/offset semantics and explicit retention behavior.
  - Red: duplicate/gap contract tests fail.
  - Verify: conformance log tests.
  - Evidence: cursor cases.

- [ ] T171 Define backend cancellation contract
  - Depends on: T169
  - Outcome: cancellation is idempotent and handles completion race.
  - Red: duplicate cancel or race test fails.
  - Verify: conformance cancellation tests.
  - Evidence: allowed outcomes.

- [ ] T172 Define backend collection contract
  - Depends on: T165, T169
  - Outcome: terminal execution exposes authorized output/result manifest and idempotent collection.
  - Red: collection contract tests fail.
  - Verify: conformance collection tests.
  - Evidence: result and output fields.

- [ ] T173 Map normalized outcome to pinned Nix `BuildResult`
  - Depends on: T010, T083, T172
  - Outcome: every required field and terminal classification has explicit mapping.
  - Red: compatibility matrix mapping test reports gap.
  - Verify: table-driven result mapping tests.
  - Evidence: complete mapping table.

- [ ] T174 Make local backend pass submit conformance
  - Depends on: T168
  - Outcome: existing local backend satisfies idempotent submit behavior.
  - Red: conformance test fails.
  - Verify: local backend conformance subset.
  - Evidence: passing submit cases.

- [ ] T175 Make local backend pass reconciliation conformance
  - Depends on: T169, T174
  - Outcome: local backend exposes stable states across restart.
  - Red: conformance test fails.
  - Verify: local reconciliation suite.
  - Evidence: passing states.

- [ ] T176 Make local backend pass log conformance
  - Depends on: T170, T175
  - Outcome: local logs obey cursor and retention rules.
  - Red: conformance test fails.
  - Verify: local log suite.
  - Evidence: cursor cases.

- [ ] T177 Make local backend pass cancellation conformance
  - Depends on: T171, T175
  - Outcome: local cancellation is idempotent and race-safe.
  - Red: conformance test fails.
  - Verify: local cancellation suite.
  - Evidence: repeated/race cases.

- [ ] T178 Make local backend pass collection conformance
  - Depends on: T172, T175
  - Outcome: local collection is idempotent and produces normalized result.
  - Red: conformance test fails.
  - Verify: local collection suite.
  - Evidence: repeated collection case.

### Gate 5 acceptance

- [ ] T179 Verify Gate 5 remote contract
  - Depends on: T156, T157, T158, T159, T160, T163, T166, T173, T174, T175, T176, T177, T178
  - Outcome: request-scoped transfer authorization is enforced and local backend passes reusable lifecycle conformance suite.
  - Red: gate script reports missing contract evidence.
  - Verify: transfer authorization, real-store collection, credential, and backend conformance suites.
  - Evidence: exact commands and pristine output.

## Gate 6 — Static SSH backend

### Configuration and capabilities

- [ ] T180 Parse static SSH backend configuration
  - Depends on: T167, T179
  - Outcome: TOML accepts name, address, host key, user, systems, features, capacities, priority, and credential reference.
  - Red: valid config test fails.
  - Verify: configuration unit test.
  - Evidence: parsed fields.

- [ ] T181 Reject duplicate backend name
  - Depends on: T180
  - Outcome: startup fails with actionable error.
  - Red: duplicate config succeeds.
  - Verify: config validation test.
  - Evidence: asserted message.

- [ ] T182 Reject impossible static SSH capacity
  - Depends on: T180
  - Outcome: zero/negative/inconsistent capacities fail.
  - Red: invalid config succeeds.
  - Verify: config validation tests.
  - Evidence: invalid cases.

- [ ] T183 Validate SSH host key configuration
  - Depends on: T180
  - Outcome: missing or mismatched host key fails closed.
  - Red: fixture connects without trusted host key.
  - Verify: real SSH host-key tests.
  - Evidence: accepted and rejected keys.

### Real SSH builder fixture

- [ ] T184 Provision real restricted SSH builder VM
  - Depends on: T183
  - Outcome: reproducible VM exposes only required Nix build service behavior.
  - Red: VM fixture cannot connect/build.
  - Verify: fixture smoke test.
  - Evidence: VM config and connection.

- [ ] T185 Reject general shell on builder credential
  - Depends on: T184
  - Outcome: Telchar credential cannot execute arbitrary shell command.
  - Red: arbitrary command succeeds.
  - Verify: negative SSH command test.
  - Evidence: asserted denial.

- [ ] T186 Reject forwarding on builder credential
  - Depends on: T184
  - Outcome: TCP/agent/X11 forwarding are disabled.
  - Red: forwarding succeeds.
  - Verify: negative forwarding tests.
  - Evidence: denied modes.

### Backend lifecycle

- [ ] T187 Submit static SSH execution idempotently
  - Depends on: T168, T184
  - Outcome: duplicate submit for one attempt resolves to one remote execution.
  - Red: conformance test sees duplicate builds.
  - Verify: real SSH submit conformance test.
  - Evidence: one execution ID.

- [ ] T188 Stage private inputs without cache
  - Depends on: T162, T187
  - Outcome: remote builder receives exact authorized closure through direct Telchar path.
  - Red: build fails when cache disabled.
  - Verify: real private-input staging test.
  - Evidence: closure paths and cache-disabled proof.

- [ ] T189 Deny remote access to unrelated input
  - Depends on: T156, T188
  - Outcome: builder credential cannot fetch path outside request closure.
  - Red: negative fetch succeeds.
  - Verify: real SSH executor authorization test.
  - Evidence: denied path.

- [ ] T190 Reconcile static SSH pending/running/completed states
  - Depends on: T169, T187
  - Outcome: backend maps real remote state into contract states.
  - Red: conformance state tests fail.
  - Verify: real SSH reconciliation suite.
  - Evidence: observed states.

- [ ] T191 Stream static SSH logs with cursor
  - Depends on: T170, T190
  - Outcome: build logs survive polling/reconnect without duplicate or gap under contract assumptions.
  - Red: conformance log test fails.
  - Verify: real SSH log suite.
  - Evidence: cursor sequence.

- [ ] T192 Cancel static SSH execution
  - Depends on: T171, T190
  - Outcome: administrative cancellation is idempotent and restricted.
  - Red: long build continues or duplicate cancel errors inconsistently.
  - Verify: real SSH cancellation test.
  - Evidence: remote state and audit.

- [ ] T193 Collect static SSH outputs
  - Depends on: T165, T172, T190
  - Outcome: declared outputs return to gateway store and validate.
  - Red: end-to-end collection fails.
  - Verify: real SSH output collection test.
  - Evidence: gateway-valid output.

- [ ] T194 Distinguish remote build failure
  - Depends on: T193
  - Outcome: derivation failure maps to terminal build failure without retry.
  - Red: failure maps to infrastructure error or retries.
  - Verify: real failing derivation test.
  - Evidence: classification and one attempt.

- [ ] T195 Distinguish SSH transport failure
  - Depends on: T190
  - Outcome: connection failure maps to infrastructure classification and transition table.
  - Red: maps to build failure.
  - Verify: controlled network failure test.
  - Evidence: classification.

- [ ] T196 Distinguish input staging failure
  - Depends on: T188
  - Outcome: unavailable/corrupt input maps to input failure.
  - Red: maps to build or internal failure.
  - Verify: controlled staging failure test.
  - Evidence: classification.

- [ ] T197 Distinguish output collection failure
  - Depends on: T193
  - Outcome: missing/corrupt remote output maps to output failure.
  - Red: reports success or build failure.
  - Verify: controlled collection fault test.
  - Evidence: classification.

- [ ] T198 Recover static SSH execution after daemon restart
  - Depends on: T190, T191, T193
  - Outcome: known attempt reconciles and completes without duplicate execution.
  - Red: restart starts second build or loses result.
  - Verify: real restart during remote build.
  - Evidence: one attempt/execution and valid output.

### Gate 6 acceptance

- [ ] T199 Verify Gate 6 static SSH backend
  - Depends on: T185, T186, T187, T188, T189, T190, T191, T192, T193, T194, T195, T196, T197, T198
  - Outcome: stock client receives verified output from real restricted SSH builder with direct private-input fallback and correct lifecycle classification.
  - Red: gate script reports missing behavior.
  - Verify: full static SSH VM suite.
  - Evidence: exact command and pristine output.

## Gate 7 — Nomad batch backend

### Isolation and job policy decision

- [ ] T200 Document minimum Nomad executor contract
  - Depends on: T199
  - Outcome: ADR selects task driver, dedicated node constraints, Nix sandbox, filesystem/network policy, resource defaults, runtime limit, secrets, gateway access, cleanup, and drain behavior.
  - Red: security checklist exposes unspecified control.
  - Verify: ADR checklist.
  - Evidence: approved minimum contract.

- [ ] T201 Define Telchar-owned Nomad retry policy
  - Depends on: T135, T200
  - Outcome: job restart/reschedule settings prevent untracked duplicate execution; drain deadline behavior is explicit.
  - Red: rendered job permits conflicting Nomad retry.
  - Verify: job-policy unit tests.
  - Evidence: restart/reschedule/drain settings.

- [ ] T202 Provision isolated Nomad development fixture
  - Depends on: T200
  - Outcome: reproducible local or VM Nomad server/client supports required task driver and cleanup.
  - Red: fixture smoke test fails or leaks jobs/allocations.
  - Verify: Nomad fixture start/run/teardown.
  - Evidence: versions, node class, cleanup.

### Configuration and job rendering

- [ ] T203 Parse Nomad backend configuration
  - Depends on: T167, T202
  - Outcome: TOML accepts endpoint, namespace, region, node constraints, capacities, resource defaults, and credential references.
  - Red: valid config test fails.
  - Verify: config tests.
  - Evidence: parsed fields.

- [ ] T204 Reject unsafe Nomad configuration
  - Depends on: T203
  - Outcome: missing isolation constraint, unbounded runtime, or missing resource defaults fail startup.
  - Red: unsafe config succeeds.
  - Verify: config validation tests.
  - Evidence: actionable messages.

- [ ] T205 Render attempt-derived Nomad job identity
  - Depends on: T168, T203
  - Outcome: job name/meta derive deterministically from attempt ID and request ID.
  - Red: duplicate render differs or collides.
  - Verify: rendering tests.
  - Evidence: stable identifiers.

- [ ] T206 Render system and feature constraints
  - Depends on: T205
  - Outcome: requested system/features map to explicit Nomad constraints.
  - Red: incompatible node remains eligible.
  - Verify: job rendering tests.
  - Evidence: constraints.

- [ ] T207 Render CPU, memory, disk, and runtime limits
  - Depends on: T200, T205
  - Outcome: every job carries configured bounded resources and timeout.
  - Red: rendered job omits a bound.
  - Verify: job rendering tests.
  - Evidence: resource fields.

- [ ] T208 Render request-scoped credential delivery
  - Depends on: T155, T205
  - Outcome: allocation receives only request credential and no organization-wide cache publisher secret.
  - Red: secret inspection test finds broad credential.
  - Verify: rendered job and live allocation inspection.
  - Evidence: secret names/absence.

- [ ] T209 Render Telchar-owned restart and reschedule policy
  - Depends on: T201, T205
  - Outcome: job spec has explicit restart/reschedule settings matching attempt accounting.
  - Red: default policy remains.
  - Verify: rendering test.
  - Evidence: policy fields.

### Backend lifecycle

- [ ] T210 Submit Nomad batch job idempotently
  - Depends on: T202, T205, T209
  - Outcome: duplicate submit for one attempt resolves to one job.
  - Red: conformance test finds duplicate jobs.
  - Verify: real Nomad submit test.
  - Evidence: one job ID.

- [ ] T211 Reconcile pending allocation
  - Depends on: T169, T210
  - Outcome: unplaced job maps to backend-pending and respects pending limit.
  - Red: pending maps to running or disappears.
  - Verify: zero-eligible-node test.
  - Evidence: Nomad and Telchar states.

- [ ] T212 Reconcile running allocation
  - Depends on: T211
  - Outcome: placed task maps to running with allocation identity.
  - Red: state mapping fails.
  - Verify: real running job test.
  - Evidence: allocation ID.

- [ ] T213 Reconcile successful allocation
  - Depends on: T212
  - Outcome: completed task maps to collecting, not direct success.
  - Red: request succeeds before output validation.
  - Verify: real completion test.
  - Evidence: collecting transition.

- [ ] T214 Reconcile failed allocation
  - Depends on: T212
  - Outcome: allocation/task failure evidence maps through failure matrix.
  - Red: failure classification test fails.
  - Verify: controlled Nomad task failure.
  - Evidence: raw and normalized states.

- [ ] T215 Reconcile missing or purged allocation
  - Depends on: T210
  - Outcome: absent backend object enters explicit absent/ambiguous policy.
  - Red: daemon blindly resubmits.
  - Verify: purged-job reconciliation test.
  - Evidence: no duplicate submission.

- [ ] T216 Stage Nomad private inputs without cache
  - Depends on: T162, T208, T212
  - Outcome: allocation obtains exact authorized closure from Telchar with cache disabled.
  - Red: build fails or accesses unrelated path.
  - Verify: private-input Nomad test.
  - Evidence: closure and cache-disabled proof.

- [ ] T217 Deny Nomad allocation unrelated path access
  - Depends on: T156, T216
  - Outcome: allocation credential cannot fetch unrelated store path.
  - Red: negative access succeeds.
  - Verify: live allocation authorization test.
  - Evidence: denied path.

- [ ] T218 Stream Nomad allocation logs with cursor
  - Depends on: T170, T212
  - Outcome: allocation logs map to contract cursor without gaps/duplicates under retention policy.
  - Red: log conformance test fails.
  - Verify: real Nomad log suite.
  - Evidence: cursor sequence.

- [ ] T219 Cancel pending Nomad job
  - Depends on: T171, T211
  - Outcome: cancellation is idempotent and releases request credential.
  - Red: pending job remains or credential works.
  - Verify: pending cancellation test.
  - Evidence: job and credential states.

- [ ] T220 Cancel running Nomad allocation
  - Depends on: T171, T212
  - Outcome: cancellation stops allocation according to race policy and revokes access.
  - Red: allocation or credential remains active.
  - Verify: running cancellation test.
  - Evidence: terminal state and revocation.

- [ ] T221 Collect Nomad output
  - Depends on: T164, T165, T213
  - Outcome: allocation uploads declared output; gateway verifies and imports it.
  - Red: end-to-end collection fails.
  - Verify: real Nomad build/collection test.
  - Evidence: gateway-valid output.

- [ ] T222 Reject Nomad undeclared output
  - Depends on: T157, T221
  - Outcome: allocation cannot publish extra path.
  - Red: extra upload succeeds.
  - Verify: live negative upload test.
  - Evidence: denied output.

- [ ] T223 Recover pending Nomad job after daemon restart
  - Depends on: T211
  - Outcome: restart finds existing job and does not duplicate it.
  - Red: second job appears.
  - Verify: restart pending test.
  - Evidence: one job ID.

- [ ] T224 Recover running Nomad allocation after daemon restart
  - Depends on: T212, T218
  - Outcome: restart resumes reconciliation/log collection without duplicate job.
  - Red: second job or lost state.
  - Verify: restart running test.
  - Evidence: one job/allocation and eventual outcome.

- [ ] T225 Recover collecting Nomad output after daemon restart
  - Depends on: T221
  - Outcome: collection resumes idempotently.
  - Red: duplicate/corrupt import or lost success.
  - Verify: collection crash-point test.
  - Evidence: one valid output.

### Autoscaling and drain evidence

- [ ] T226 Export pending Nomad demand metric through OTLP
  - Depends on: T009E, T211
  - Outcome: bounded OTLP metric reports submitted pending demand by approved capability class and links operational traces without request identity attributes.
  - Red: collector test lacks the metric or exposes request identity/high-cardinality attributes.
  - Verify: Nomad demand OTLP metrics collector test.
  - Evidence: instrument name, unit, and attributes.

- [ ] T227 Prove pending job runs when eligible node appears
  - Depends on: T211, T212
  - Outcome: job remains pending at zero eligible capacity and runs after fixture adds eligible client.
  - Red: job fails or requires Telchar resubmission.
  - Verify: scale-from-zero fixture test.
  - Evidence: same job ID pending then running.

- [ ] T228 Prove drain blocks new allocation
  - Depends on: T202, T212
  - Outcome: draining client receives no new job.
  - Red: new allocation lands on drained node.
  - Verify: drain placement test.
  - Evidence: placement result.

- [ ] T229 Prove configured drain preserves running build
  - Depends on: T201, T228
  - Outcome: running batch completes under configured drain policy before node removal.
  - Red: drain deadline kills build.
  - Verify: long-build drain test.
  - Evidence: completion and drain settings.

### Gate 7 acceptance

- [ ] T230 Verify Gate 7 Nomad backend
  - Depends on: T204, T210, T211, T212, T213, T214, T215, T216, T217, T218, T219, T220, T221, T222, T223, T224, T225, T226, T227, T228, T229
  - Outcome: one real batch job per attempt executes with bounded isolation, direct private-input fallback, idempotent reconciliation, correct cancellation, restart recovery, and external scaling compatibility.
  - Red: gate script reports missing behavior.
  - Verify: full isolated Nomad integration suite.
  - Evidence: exact commands and pristine output.

## Gate 8 — Optional binary-cache integration

### Cache boundary and lookup

- [ ] T231 Define cache policy configuration
  - Depends on: T230
  - Outcome: TOML separates substituters, lookup timeout, publication enablement, publication policy, and secret file references.
  - Red: config tests fail.
  - Verify: cache config tests.
  - Evidence: valid/invalid examples.

- [ ] T232 Reject plaintext cache credential
  - Depends on: T231
  - Outcome: startup rejects credential values embedded in main configuration.
  - Red: plaintext secret config succeeds.
  - Verify: config negative test.
  - Evidence: asserted message.

- [ ] T233 Look up expected output in substituter
  - Depends on: T231
  - Outcome: bounded read-through lookup checks one expected output before dispatch.
  - Red: real cache-hit test still dispatches backend.
  - Verify: real binary-cache fixture test.
  - Evidence: hit and zero executions.

- [ ] T234 Continue to execution on cache miss
  - Depends on: T233
  - Outcome: miss preserves direct build path.
  - Red: request fails or stalls.
  - Verify: cache-miss end-to-end test.
  - Evidence: one backend execution and valid output.

- [ ] T235 Fail open on cache lookup timeout
  - Depends on: T233
  - Outcome: timeout is bounded and dispatch proceeds.
  - Red: build waits beyond configured bound or fails.
  - Verify: stalled-cache integration test.
  - Evidence: measured timeout and successful build.

- [ ] T236 Fail open on cache outage
  - Depends on: T233
  - Outcome: unreachable cache does not prevent valid build.
  - Red: request fails.
  - Verify: cache-outage test.
  - Evidence: cache error and build success.

- [ ] T237 Verify substituted output before success
  - Depends on: T233
  - Outcome: cache hit imports into real gateway store and passes same output validation as backend result.
  - Red: corrupt cache fixture reports success.
  - Verify: valid/corrupt cache tests.
  - Evidence: accepted and rejected outputs.

### Executor substitution

- [ ] T238 Permit executor public-cache substitution
  - Depends on: T216, T231
  - Outcome: executor may fetch authorized closure paths from configured read cache.
  - Red: cache-enabled fixture performs direct transfer for every public path.
  - Verify: real executor substitution test.
  - Evidence: substituted path and transfer counts.

- [ ] T239 Fall back to Telchar for missing private input
  - Depends on: T238
  - Outcome: cache miss for private input uses request-scoped direct transfer.
  - Red: build fails with private input absent from cache.
  - Verify: mixed public/private input test.
  - Evidence: cache hit plus direct fallback.

- [ ] T240 Keep executor cache credential read-only
  - Depends on: T238
  - Outcome: executor cannot publish to shared cache.
  - Red: live executor publication attempt succeeds.
  - Verify: negative credential test.
  - Evidence: denied operation.

### Asynchronous publication

- [ ] T241 Create output publication record
  - Depends on: T231, T107
  - Outcome: successful build can enqueue durable publication job without changing build outcome.
  - Red: publication schema/transaction test fails.
  - Verify: real PostgreSQL publication-operation test.
  - Evidence: request/output/policy fields.

- [ ] T242 Acquire publication output lease
  - Depends on: T241, T075
  - Outcome: publication job independently retains output until terminal publication state.
  - Red: GC removes queued publication output.
  - Verify: real GC publication lease test.
  - Evidence: path validity and lease owner.

- [ ] T243 Publish allowed output asynchronously
  - Depends on: T242
  - Outcome: worker publishes policy-approved output after client success.
  - Red: real cache fixture lacks output.
  - Verify: asynchronous publication test.
  - Evidence: client completion precedes publication completion.

- [ ] T244 Do not publish client-uploaded inputs
  - Depends on: T243
  - Outcome: publication manifest contains only approved outputs.
  - Red: cache fixture receives source/input path.
  - Verify: privacy negative test.
  - Evidence: absent input paths.

- [ ] T245 Respect output publication policy
  - Depends on: T243
  - Outcome: denied identity/project/output policy creates no publication.
  - Red: disallowed output appears in cache.
  - Verify: policy table tests with real cache fixture.
  - Evidence: allowed and denied cases.

- [ ] T246 Retry publication independently
  - Depends on: T243
  - Outcome: bounded publication retry does not mutate successful build outcome.
  - Red: outage changes request to failed or retries unboundedly.
  - Verify: flaky-cache publication test.
  - Evidence: request success and publication attempts.

- [ ] T247 Release publication lease after terminal publication
  - Depends on: T242, T246
  - Outcome: success or exhausted failure releases lease according to retention policy.
  - Red: lease leaks or releases before retry ends.
  - Verify: publication lifecycle test.
  - Evidence: state/root transitions.

- [ ] T248 Recover queued publication after daemon restart
  - Depends on: T241, T243
  - Outcome: restart resumes publication without duplicate harmful side effects.
  - Red: job is lost or duplicated unexpectedly.
  - Verify: restart publication test.
  - Evidence: attempt history and cache output.

- [ ] T249 Export cache telemetry through OTLP
  - Depends on: T009E, T233, T243, T246
  - Outcome: cache lookup/publication spans, structured events, and OTLP metrics cover hit/miss/timeout/error/publication outcomes with bounded attributes.
  - Red: collector test misses a cache signal or leaks URL/identity attributes.
  - Verify: cache OTLP logs, metrics, and traces integration test.
  - Evidence: instruments, spans, events, and allowed attributes.

### Gate 8 acceptance

- [ ] T250 Verify Gate 8 optional cache
  - Depends on: T234, T235, T236, T237, T239, T240, T244, T245, T246, T247, T248, T249
  - Outcome: cache improves hits and publication while outage/miss/private inputs preserve direct correctness path.
  - Red: gate script reports missing outage/privacy evidence.
  - Verify: full real-cache integration suite.
  - Evidence: exact commands and pristine output.

## Gate 9 — Release hardening

### Configuration and service behavior

- [ ] T251 Parse complete server configuration
  - Depends on: T250
  - Outcome: server, quota, state, backend, transfer, logging, metrics, and cache sections compose into validated startup config.
  - Red: complete config fixture fails.
  - Verify: configuration integration tests.
  - Evidence: supported fields.

- [ ] T252 Reject unknown configuration field
  - Depends on: T251
  - Outcome: typo fails startup with exact field path.
  - Red: unknown field ignored.
  - Verify: negative config test.
  - Evidence: actionable message.

- [ ] T253 Reject duplicate backend names across kinds
  - Depends on: T251
  - Outcome: local/SSH/Nomad names share one unique namespace.
  - Red: duplicate succeeds.
  - Verify: config negative test.
  - Evidence: asserted error.

- [ ] T254 Reject unsupported public capability envelope
  - Depends on: T251, T129
  - Outcome: startup fails when configured public systems/features have no eligible backend.
  - Red: impossible envelope starts.
  - Verify: config capability test.
  - Evidence: missing combinations.

- [ ] T255 Load secrets from files with permission checks
  - Depends on: T251
  - Outcome: service loads referenced credentials and rejects unsafe file permissions where enforceable.
  - Red: broad-permission fixture succeeds.
  - Verify: secret-file tests.
  - Evidence: allowed/denied modes.

- [ ] T256 Handle graceful daemon shutdown
  - Depends on: T151
  - Outcome: daemon stops admission, checkpoints state, leaves running backend work reconcilable, and exits within bound.
  - Red: shutdown loses state or hangs.
  - Verify: long-build shutdown/restart test.
  - Evidence: timing and recovered state.

- [ ] T257 Handle backend drain state
  - Depends on: T167, T199, T230
  - Outcome: drained backend receives no new attempts while existing attempts reconcile.
  - Red: scheduler dispatches new work to drained backend.
  - Verify: drain scheduler/integration tests.
  - Evidence: existing/new request behavior.

### Security and robustness

- [ ] T258 Add protocol corpus regression suite
  - Depends on: T045
  - Outcome: captured valid/malformed/version-boundary frames run in ordinary tests.
  - Red: corpus runner missing cases.
  - Verify: corpus test command.
  - Evidence: corpus inventory.

- [ ] T259 Add structured parser fuzz CI smoke
  - Depends on: T030, T258
  - Outcome: bounded fuzz smoke runs in reproducible environment.
  - Red: CI/check target absent.
  - Verify: flake fuzz-smoke check.
  - Evidence: duration and corpus growth policy.

- [ ] T260 Enforce bounded audit and log field lengths
  - Depends on: T147, T150
  - Outcome: attacker-controlled values cannot create unbounded records or terminal escapes.
  - Red: oversized/control-character test corrupts output.
  - Verify: structured log/audit tests.
  - Evidence: sanitization and bounds.

- [ ] T261 Redact credentials from logs and errors
  - Depends on: T155, T255
  - Outcome: token/key/secret fixtures never appear in captured logs or client errors.
  - Red: secret scanner finds fixture secret.
  - Verify: end-to-end redaction test.
  - Evidence: zero matches.

- [ ] T262 Enforce build runtime timeout
  - Depends on: T080, T192, T220
  - Outcome: each backend stops or classifies work exceeding configured runtime.
  - Red: timeout fixture runs beyond bound.
  - Verify: local/SSH/Nomad timeout tests.
  - Evidence: durations and classifications.

- [ ] T263 Enforce log retention limit
  - Depends on: T170
  - Outcome: retained logs obey byte/time policy while terminal outcome remains available.
  - Red: log store exceeds bound.
  - Verify: retention cleanup test.
  - Evidence: before/after sizes.

- [ ] T264 Enforce PostgreSQL retention policy
  - Depends on: T101, T150
  - Outcome: domain-specific cleanup operation removes old terminal operational records according to policy without breaking audit requirements, active references, or lease constraints.
  - Red: cleanup deletes active/leased data or retains expired data.
  - Verify: real PostgreSQL retention-operation test.
  - Evidence: retained/deleted rows and transaction boundary.

- [ ] T265 Run dependency and source-provenance audit
  - Depends on: T250
  - Outcome: repository records allowed dependency licenses, confirms `nix-worker-protocol` source provenance, and validates notices for any separately approved future import.
  - Red: audit reports unknown/disallowed package, untracked copied source, or missing notice.
  - Verify: reproducible license and provenance audit command.
  - Evidence: report path and zero unresolved findings.

- [ ] T266 Run dependency vulnerability audit
  - Depends on: T250
  - Outcome: reproducible Rust/Nix dependency audit has no unresolved applicable critical/high findings.
  - Red: audit fixture or current graph reports findings.
  - Verify: documented audit command.
  - Evidence: report and dispositions.

### Compatibility and load

- [ ] T267 Re-run pinned Nix compatibility matrix
  - Depends on: T250, T258
  - Outcome: every supported matrix cell has passing real-client evidence.
  - Red: matrix validator finds stale/missing result.
  - Verify: compatibility suite.
  - Evidence: versions, cells, commands.

- [ ] T268 Add next Nix version compatibility candidate
  - Depends on: T267
  - Outcome: one additional stock Nix version is tested and marked supported or rejected with evidence; no silent compatibility claim.
  - Red: candidate matrix cell unresolved.
  - Verify: version-specific suite.
  - Evidence: status and differences.

- [ ] T269 Evaluate one Lix compatibility candidate
  - Depends on: T267
  - Outcome: one pinned Lix version is tested and marked supported or deferred with evidence.
  - Red: Lix status remains assumption.
  - Verify: Lix compatibility suite or documented blocker reproduction.
  - Evidence: status and trace differences.

- [ ] T270 Load-test global queue bounds
  - Depends on: T151
  - Outcome: concurrent real clients cannot exceed configured queued/active limits and receive clean rejections.
  - Red: load fixture violates counters or leaks sessions.
  - Verify: bounded load test.
  - Evidence: concurrency, limits, observed maxima.

- [ ] T271 Load-test scheduler fairness
  - Depends on: T134, T270
  - Outcome: multiple quota subjects progress according to deterministic policy under sustained demand.
  - Red: measured sequence violates fairness bound.
  - Verify: scheduler load test.
  - Evidence: dispatch distribution.

- [ ] T272 Load-test transfer backpressure
  - Depends on: T069, T162, T270
  - Outcome: slow clients/executors remain within memory, disk, and connection bounds.
  - Red: resource measurement exceeds limits.
  - Verify: transfer load test.
  - Evidence: peak resource use.

- [ ] T273 Soak-test restart reconciliation
  - Depends on: T198, T224, T248
  - Outcome: repeated daemon restarts during mixed backend/cache activity produce no duplicate active attempts or lost terminal records.
  - Red: soak invariant checker finds violation.
  - Verify: bounded restart soak command.
  - Evidence: iterations and zero invariant failures.

### Packaging and operations

- [ ] T274 Build reproducible Telchar package
  - Depends on: T250
  - Outcome: flake produces versioned daemon/CLI artifact from clean checkout.
  - Red: package build fails or embeds dirty state.
  - Verify: `nix build` package target twice and compare declared reproducibility evidence.
  - Evidence: store paths/hashes.

- [ ] T275 Add NixOS module for single-active deployment
  - Depends on: T251, T255, T274
  - Outcome: module configures daemon, PostgreSQL connection/credentials, local IPC, service user, state directory, OTLP endpoint/security/credential references, local telemetry formatting, resource attributes, and OpenSSH forced command without broad shell access. PostgreSQL and the OTLP collector may be local or external according to deployment configuration.
  - Red: NixOS VM module test fails.
  - Verify: module evaluation and VM test.
  - Evidence: service and sshd assertions.

- [ ] T276 Add PostgreSQL service upgrade migration test
  - Depends on: T096, T275
  - Outcome: previous PostgreSQL schema fixture upgrades and preserves active/terminal records.
  - Red: upgrade loses or corrupts data.
  - Verify: NixOS upgrade VM test against real PostgreSQL.
  - Evidence: PostgreSQL versions, before/after schema, and preserved records.

- [ ] T277 Document operator deployment
  - Depends on: T275
  - Outcome: docs cover trust assumptions, client builder envelope/maxJobs, host keys, identity mapping, stores/GC, quotas, backends, secrets, metrics, backup, upgrade, drain, and recovery.
  - Red: operator checklist finds missing required topic.
  - Verify: documentation command/checklist and tested examples.
  - Evidence: linked sections and command results.

- [ ] T278 Document security model and non-goals
  - Depends on: T260, T261, T265, T266
  - Outcome: public docs clearly state shared-store visibility, trusted executors, untrusted derivations, no hostile tenant isolation, and no provenance proof for classic outputs.
  - Red: terminology checker finds conflicting privacy/security claim.
  - Verify: documentation consistency test.
  - Evidence: exact documented assumptions.

- [ ] T279 Document disaster recovery
  - Depends on: T256, T276
  - Outcome: runbook covers PostgreSQL backup/restore, point-in-time assumptions, gateway-store coordination, ambiguous attempts, backend reconciliation, and publication recovery.
  - Red: tabletop checklist exposes missing recovery step.
  - Verify: scripted PostgreSQL backup/restore rehearsal against the gateway-store fixture.
  - Evidence: restored state and reconciled work.

- [ ] T280 Add release verification script
  - Depends on: T265, T266, T267, T270, T271, T272, T273, T274, T275, T276
  - Outcome: one external-monitor command runs all mandatory release checks or clearly orchestrates documented privileged suites.
  - Red: script reports missing suite/artifact.
  - Verify: release verification command from fresh shell.
  - Evidence: exact environment and clean output summary.

### Gate 9 acceptance

- [ ] T281 Verify release candidate
  - Depends on: T280, T277, T278, T279
  - Outcome: pinned supported client builds through local, static SSH, and Nomad backends; optional cache failure is harmless; security, load, restart, packaging, and documentation gates pass.
  - Red: release checklist reports any unresolved blocker or dirty output.
  - Verify: release verification script and documented privileged integration commands.
  - Evidence: immutable report paths, versions, commands, and residual limitations.

## Explicitly deferred work

These items require separate design review before implementation and are not hidden inside the tasks above:

- Multiple active Telchar gateways or scheduler high availability.
- Supporting a durable database other than PostgreSQL.
- Multiple active scheduler ownership and distributed dispatch fencing.
- Hostile client multi-tenancy or per-path client authorization.
- Per-tenant gateway stores.
- Reproducible-build consensus or cryptographic provenance for classic input-addressed outputs.
- Duplicate active request coalescing.
- Group reservations, weighted shares, or cost research scheduler behavior.
- Kubernetes, cloud batch, or dedicated worker-protocol backends.
- Extracting `nix-worker-protocol` into a separate repository or published external dependency before its API survives another Nix/Lix target, contains no Telchar domain types, has independently runnable compatibility/property/fuzz suites, has a real second consumer, and has documented versioning, ownership, release, maintenance, and provenance processes.
- Provider-specific machine provisioning or autoscaling.
- Full client reattachment and log resumption after transport loss.
- Interactive shell access.

## Ralph start guidance

Do not start one loop for the entire file. Start at the first unchecked task whose dependencies are complete. Copy that task and its gate context into `.ralph/<task-id>-<slug>.md`. Suggested loop settings:

```text
itemsPerIteration = 1
reflectEvery = 5
maxIterations = 20
```

Decision/prototype tasks may require fewer iterations. VM, Nomad, load, or soak tasks may require more, but their task scope must not be widened. If a task exposes an unrecorded architectural choice, stop it, add a decision task to this plan, and block dependent work rather than deciding incidentally.
