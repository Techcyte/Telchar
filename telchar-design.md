# Telchar Design Brief

**Status:** Reviewed design baseline; implementation not started

Telchar is a self-hosted Nix build gateway. It accepts ordinary Nix remote-build requests over SSH, applies admission control and scheduling, and dispatches each derivation to a compatible execution backend.

The project is named after Telchar, the renowned smith of Nogrod in Tolkien's legendarium. The name is thematic; public APIs and operator terminology should remain straightforward and domain-focused.

## Project summary

Telchar presents one stable Nix remote-builder endpoint to users:

```text
Stock Nix client
      │
      │ SSH + Nix worker protocol
      ▼
Telchar
      │
      ├── admission and quotas
      ├── queueing and scheduling
      ├── input and output transfer
      ├── execution tracking
      └── result and log streaming
             │
             ▼
      Execution backend
       ├── local Nix
       ├── static SSH builder
       ├── Nomad batch job
       └── future provider
```

Users keep using standard Nix commands and standard remote-builder configuration. They do not install a custom client, patch Nix, or learn the topology of the build fleet.

Telchar decomposes the system into small responsibilities:

- Nix remains responsible for evaluation and deciding which derivations are ready to build.
- Telchar accepts buildable derivations, enforces policy, and chooses an execution backend.
- A backend executes one derivation and reports its result.
- External infrastructure manages machine provisioning and autoscaling.
- An optional binary cache accelerates transfers and shares completed outputs, but is not required for correctness.

## Reviewed design constraints

The design review established the following initial constraints. These are implementation boundaries, not aspirations.

### Initial deployment and trust boundary

The first release is single-active: one Telchar daemon owns scheduling, durable state, gateway-store coordination, and backend reconciliation. The daemon must enforce this constraint by acquiring one stable PostgreSQL advisory lock on a dedicated lifetime connection before activating admission, scheduling, reconciliation, or mutating administrative work. A second daemon sharing the database must fail startup. Loss of the lock connection fences the daemon immediately: it stops admission and all new external or durable side effects, emits operator-visible telemetry, and exits within a bound so a replacement can perform normal restart reconciliation.

This singleton lock prevents accidental split brain; it is not leader election or high availability. Active/passive failover is post-MVP work because it additionally requires leadership epochs, protocol-session routing, backend dispatch fencing, gateway-store availability on the standby, and failure-injection proof. Active/active scheduling remains out of scope.

All authenticated clients in the first release belong to one mutually trusted store domain. They may be able to query or download any path present in the shared gateway store when they know its store path. Path opacity is not an authorization boundary. Hostile client multi-tenancy, per-tenant stores, and per-path client authorization are deferred.

Build payloads are untrusted and must be sandboxed. Executor hosts and their Nix daemons are trusted for build-result integrity. Telchar verifies transport and Nix-store invariants, expected outputs, NAR metadata, references, and content-addressed hashes where applicable; it cannot prove that a classic input-addressed output was honestly produced. Reproducible rebuild or consensus verification is outside the initial scope.

### Process topology

The initial topology uses OpenSSH as the network-facing SSH implementation. A restricted forced command starts one `telchar serve-stdio` frontend per connection. The frontend owns only the protocol stream and an attachment to a build request; it communicates over authenticated local IPC with the single Telchar daemon.

The daemon owns:

- Shared admission and scheduler state.
- Durable request and execution-attempt records.
- Gateway-store leases and operations.
- Backend submission and reconciliation.
- Administrative state and metrics.

The frontend must not create an independent scheduler or connect to PostgreSQL directly. Trusted SSH authentication metadata must come from OpenSSH-controlled data, not client-supplied environment variables. The exact OpenSSH identity handoff, including public keys and certificates, is a prototype gate. If OpenSSH cannot provide the required authenticated metadata, ingress design must be revisited before identity or quota work begins.

### Compatibility boundary

The first compatibility target is exactly the stock Nix version pinned by Telchar's Nix flake. Lix and additional Nix versions are added only after real-client compatibility tests pass. Telchar will maintain an explicit matrix covering client version, worker-protocol version, trust mode, required operations, content-addressed behavior, and support status.

Stock remote-building traffic is not guaranteed to use only `BuildDerivation`. Depending on client version, trust negotiation, and derivation type, Nix may use operations such as `BuildPathsWithResults`. The protocol spike must capture the actual operation sequence and define an allowlist for the supported compatibility target. Recognized but unsupported operations must fail deterministically with a Nix-compatible error.

### Client scheduling contract

Telchar schedules only work submitted by client Nix daemons. It cannot observe derivations retained in a client's ready queue. The builder entry's `maxJobs` is ingress credit controlling how much work a client may submit concurrently; it is not Telchar backend capacity.

Each deployment must publish a deliberate capability envelope of systems and supported features. Client builder configuration must match that envelope. Unsupported capability combinations fail promptly rather than waiting forever. Queue and autoscaling metrics represent admitted, submitted demand, not all work potentially ready on client machines.

### Store ownership and retention

The first deployment uses a dedicated gateway host or VM whose system Nix store is controlled by Telchar and not shared with unrelated host workloads. Alternate store roots remain a prototype topic until real Nix behavior proves them necessary and supportable.

Every accepted build holds durable GC roots or equivalent leases for its derivation and complete required input closure. Imported outputs remain retained until result-delivery and detachment policy permit release. Optional best-effort cache publication relies only on the output's ordinary post-result retention window and does not extend correctness-critical retention. Lease release and terminal state changes must be coordinated so garbage collection cannot invalidate queued, running, collecting, or deliverable work.

### Request and execution invariants

Protocol sessions, build requests, request attachments, execution attempts, and terminal outcomes are distinct records.

- One accepted build operation maps to one build request.
- A request may have multiple attachments and bounded sequential attempts.
- Each attempt receives a durable ID and backend idempotency key before submission.
- Backend object names derive from the attempt ID where possible.
- Only one unfenced active attempt may exist for a request.
- Retry creates a linked attempt; it never mutates terminal attempt history.
- Terminal states are immutable.
- Recovery reconciles ambiguous submission before any resubmission.
- Dispatching, backend-pending, running, and collecting have explicit independent limits.

The initial retry policy is conservative: no automatic retry unless the failure point is classified as infrastructure-related, the previous attempt is known inactive or fenced, and the retry is explicitly allowed by a tested transition table. For Nomad, Telchar owns retry policy; Nomad restart and reschedule behavior must be configured to avoid untracked duplicate execution.

### Two-layer admission

Admission has two boundaries:

1. Session and transfer admission limits connections, protocol frame and string sizes, NAR bytes, object counts, transfer rates, disk reserve, retained bytes, and log buffering before or during store operations.
2. Build admission applies global and per-quota-subject queued, dispatching, pending, and running limits when a build operation arrives.

Minimum global transfer bounds are part of the local vertical slice. Identity-specific quotas and fair scheduling follow trusted identity propagation and durable state.

### Identity model

Authentication credential, audit attribution, and quota ownership are separate concepts:

```text
Requester
├── credential ID
├── audit subject
├── quota subject
├── certificate issuer and principals, when present
└── source address metadata
```

A credential ID is scoped by its authentication authority. An audit subject is the configured owner or CA-scoped certificate identity. A quota subject is an explicit configured mapping and falls back to credential ID. A bare source IP is audit context and emergency classification only.

### Worker-protocol crate and source policy

The Nix worker protocol is implemented in a dedicated workspace crate named `nix-worker-protocol` (`nix_worker_protocol` in Rust source). The name describes the public protocol rather than Telchar, allowing other projects to use it without adopting Telchar terminology. `telchar-nix` is intentionally avoided because it is project-branded and too broad to communicate the crate's boundary.

The crate owns only reusable Nix wire behavior:

- Bounded wire primitives and framing.
- Protocol versions and feature negotiation.
- Worker operation codes and request/response types.
- Structured activity, log, and error frames.
- Nix build-result wire representations.
- Protocol compatibility fixtures, property tests, and fuzz targets.

It must not depend on Telchar identity, admission, scheduling, PostgreSQL state, gateway-store leases, SSH ingress, execution backends, cache policy, or service configuration. Telchar adapts the crate's protocol requests to domain-specific state and store operations.

Initial implementation is independent: behavior is derived from captured stock-Nix traffic, primary Nix source and documentation, and real-client compatibility tests. Rio-build and `rio-nix` may inform architecture, terminology, edge-case inventories, and test categories, but their implementation must not be copied, translated, or mechanically adapted into the initial crate. This avoids uncertain licensing provenance, unused Rio abstractions, and accidental compatibility with Rio rather than current Nix.

Transparent traffic observation uses the same bounded wire primitives and typed request/response definitions as the production protocol crate. The worker protocol has no generic request envelope, so operation boundaries must never be inferred from transport read chunks or discovered by scanning arbitrary payload bytes. Before a compatibility trace is accepted, every fixture-reachable request, response, callback, and upload flow must have an exact versioned parser derived from primary Nix serializers. The observer streams payload bodies without retaining them, records only approved bounded metadata, and fails closed when it encounters an untyped flow. Nix debug output may aid diagnosis but is not compatibility evidence.

Any future proposal to import Rio source requires a separate ADR, exact upstream revision, applicable license evidence, imported-file manifest, retained notices, local modification policy, and tests proving the import is preferable to the existing implementation. Import is an exception, not the default path.

The crate begins inside the Telchar repository so protocol and gateway changes can remain atomic while the API is unstable. Extraction to a separate repository and publication as an external dependency may be considered only after:

- The API has survived the initial supported Nix version and at least one additional Nix or Lix target.
- Its public API contains no Telchar domain types.
- Compatibility, property, and fuzz suites can run independently.
- A real second consumer exists or is ready to integrate.
- Versioning, maintenance ownership, release process, and provenance are documented.

Until those conditions hold, Telchar uses a workspace path dependency and does not consume the crate through a Git dependency.

## Goals

### Transparent Nix integration

Telchar must work with unmodified, generally available Nix clients through the standard remote-builder interface.

A client should be able to use Telchar through normal configuration such as:

```conf
builders = ssh-ng://nix-builder@telchar.example.org x86_64-linux,aarch64-linux
```

The exact URI and supported systems are deployment-specific. Telchar must not require an experimental client-side protocol, patched Nix package, wrapper command, or custom evaluator.

### Central scheduling

Telchar should give all connected clients one shared scheduling point. The scheduler can therefore make decisions using global queue and execution state rather than the partial view available to each individual Nix daemon.

### Pluggable execution

Execution must be separate from scheduling. Telchar should be able to dispatch a derivation to:

- A local Nix installation.
- A persistent machine reachable over SSH.
- A one-shot Nomad batch job.
- A future backend without changing the client protocol or scheduler model.

Backends execute work. They do not own admission policy, user quotas, or global scheduling.

### External autoscaling

Telchar should expose enough demand and execution state for an external autoscaler, but it should not provision machines itself.

For example, a Nomad backend may create one pending batch job per derivation. Nomad and an external autoscaler can react to pending allocations by adding capacity. Telchar does not need to know whether that capacity comes from an AWS Auto Scaling Group, physical machines, another cloud, or a static cluster.

### Optional caching

A shared binary cache such as Attic should improve cache hits, staging, and output distribution when configured. Telchar must still function when no shared cache exists or when an optional cache is unavailable.

### Useful for individuals and organizations

The initial implementation should remain small enough for a single operator while establishing boundaries suitable for multiple engineers:

- Stable requester identity.
- Per-identity admission limits.
- Fair scheduling.
- Auditable executions.
- Backend isolation.
- No shared private client key.

## Non-goals

Telchar is not intended to:

- Evaluate flakes or Nix expressions.
- Replace `nix-daemon` on client machines.
- Act as a CI orchestrator.
- Receive source-control webhooks.
- Manage jobsets or build matrices.
- Provision or destroy compute instances.
- Implement an AWS autoscaler.
- Replace Nomad, Kubernetes, or another infrastructure scheduler.
- Require a particular binary cache.
- Replace all binary-cache software.
- Implement a new content-addressed object store in its first version.
- Schedule arbitrary shell commands.
- Expose interactive shell access to builders.
- Promise transparent migration of an in-progress build between executors.

## Why Telchar exists

Nix remote building provides a capable transport and execution protocol, but it is not a complete shared build-farm control plane.

Nix provides:

- Local evaluation.
- Derivation dependency traversal.
- Store-path transfer.
- Remote build delegation.
- Build logs and results.
- System and feature declarations.

A shared elastic build service additionally needs:

- Stable ingress.
- Global admission control.
- Requester quotas.
- Queue fairness.
- Backend selection.
- Central visibility into active work.
- Dynamic execution targets.
- Safe cancellation and draining.
- Infrastructure-neutral execution.
- Operational metrics and audit records.

Telchar fills this control-plane gap while retaining standard Nix clients.

## The Nix worker protocol

When Nix uses an `ssh-ng://` remote builder, it does not normally send a textual `nix build` shell command over SSH.

SSH starts a remote process, conventionally:

```text
nix-daemon --stdio
```

The local Nix daemon and remote process then exchange Nix's binary worker protocol over standard input and output. The protocol includes operations for:

- Version negotiation.
- Build options.
- Store-path validity queries.
- Path metadata queries.
- NAR uploads and downloads.
- Derivation build requests.
- Structured log and error frames.
- Build results and output metadata.

Telchar's SSH endpoint replaces `nix-daemon --stdio` with its own worker-protocol server:

```text
ForceCommand telchar serve-stdio
```

From the client's perspective, Telchar behaves as a remote Nix store and builder. Internally, Telchar can queue and dispatch the requested build elsewhere.

Worker-protocol compatibility is one of the highest-risk parts of the project. The `nix-worker-protocol` crate implements required behavior independently from captured protocol evidence, primary Nix source and documentation, and real-client tests. Rio-build is reference material for architecture and test coverage only; its implementation is not copied or translated into the initial crate. Protocol framing, NAR streaming, version differences, and structured error handling must be implemented from evidence rather than assumptions.

## Core architectural principle

An accepted derivation build operation is the unit of scheduling. An execution attempt is the unit of backend dispatch.

In standard remote-builder mode, the local Nix daemon traverses the dependency graph and submits derivations when their dependencies are available. Telchar can therefore begin without implementing a full DAG evaluator or planner.

The initial model is:

```text
one accepted build operation
        │
        ▼
one Telchar build request
        │
        ▼
one or more bounded sequential attempts
        │
        ▼
one immutable terminal outcome
```

The initial compatibility target may use `BuildDerivation`, `BuildPathsWithResults`, or both according to captured behavior from the pinned stock Nix client. The scheduling model must not depend on only one worker operation code.

A backend may use a persistent worker or create a one-shot allocation, but that difference remains behind the backend interface.

## Components

### SSH ingress

The SSH layer provides:

- Encryption.
- Server authentication.
- Client public-key or certificate authentication.
- Source address information.
- A restricted forced command.
- No shell, PTY, forwarding, or unrelated command execution.

Telchar should support ordinary SSH host keys. SSH host certificates may be configured, but must not be required.

Likewise, clients may authenticate with:

- Authorized public keys.
- OpenSSH user certificates.
- Certificates issued by Vault or another SSH CA.

The protocol and scheduler should depend on normalized requester identity, not a particular certificate issuer.

### Protocol gateway

The protocol gateway:

- Negotiates the Nix worker protocol.
- Receives store paths and derivation requests.
- Maintains the protocol session.
- Converts protocol operations into internal store and build operations.
- Streams logs and terminal results back to the client.
- Handles client disconnects without corrupting build state.

The gateway must not contain backend-specific Nomad or SSH scheduling logic.

### Gateway store

The first implementation should use a real Nix store owned or controlled by Telchar.

The gateway store is the correctness boundary for an active request. It holds:

- Uploaded derivations.
- Client-provided source and input paths.
- Inputs required by executors.
- Outputs collected from successful executions.
- Nix path metadata required to answer the client protocol.

A local Nix store avoids prematurely designing a custom CAS, NAR database, garbage collector, or path-info implementation.

A future implementation may introduce alternate storage, but the initial system should optimize for correctness and interoperability.

### Admission controller

Admission occurs before expensive execution is started.

It should enforce bounded limits such as:

- Maximum global queued requests.
- Maximum global running requests.
- Maximum queued requests per identity.
- Maximum running requests per identity.
- Optional request size or input-transfer limits.
- Optional architecture or feature policy.

A rejected request must receive a clear Nix-compatible error. It must not be silently dropped.

Build admission does not protect uploads that occur before the build operation. The gateway must separately enforce bounded connections, protocol allocations, NAR sizes, retained bytes, transfer rates, disk reserve, and bounded log buffering.

### Scheduler

The scheduler owns:

- Queue ordering.
- Fairness between identities.
- Backend compatibility filtering.
- Backend ranking.
- Dispatch.
- Execution state.
- Retry classification.
- Cancellation eligibility.

The initial scheduler should be deliberately modest. A reasonable first policy is round-robin across requester identities with per-identity concurrency limits, then backend selection by:

1. Required system.
2. Required features.
3. Administrative priority.
4. Available capacity.
5. Configured backend preference or cost.
6. Current active execution count.

This avoids both first-in global monopolization and premature work on a sophisticated weighted scheduler.

### Execution registry

Telchar needs durable or reconstructable state for:

- Request ID.
- Requester identity.
- Derivation path.
- Requested system and features.
- Queue time.
- Selected backend.
- Backend execution ID.
- Current state.
- Start and completion timestamps.
- Terminal classification.
- Output paths.
- Audit metadata.

The first implementation uses PostgreSQL as its durable control-plane database while retaining an explicit single-active Telchar daemon constraint. PostgreSQL is an infrastructure choice, not a claim of scheduler high availability. One lifetime PostgreSQL advisory lock enforces singleton ownership at runtime. Losing that database session fences the daemon rather than allowing it to continue scheduling through a partition. Multiple active gateways still require a separate design for leadership epochs, protocol-session ownership, dispatch fencing, and gateway-store coordination.

Persistence is encapsulated behind domain-specific state operations such as accepting a request, attaching a session, claiming runnable work, creating an attempt, recording backend submission, transitioning execution state, completing a request, acquiring store leases, and recovering incomplete attempts. Protocol, admission, scheduler, and backend code must not issue arbitrary SQL or depend on table-shaped generic repositories.

Database interchangeability is not an initial goal. Telchar should use PostgreSQL transactions, constraints, locking, and `RETURNING` semantics where they improve correctness. It must not weaken the design to a lowest-common-denominator database API or create generic CRUD abstractions that hide transaction ownership. Supporting another database requires separate design, migration, concurrency, recovery, and real integration-test work.

Database migrations are repository-controlled. Critical state operations own explicit transaction boundaries. Dispatch state changes and attempt creation must be transactional, and restart recovery must reconcile ambiguous backend submission before resubmitting work. Unit tests may exercise pure scheduling and transition decisions without a database, but persistence tests must use real PostgreSQL rather than SQLite substitutes or mocked repositories.

### Backend interface

A backend should implement a narrow lifecycle:

```text
submit(request) -> execution
status(execution) -> pending | running | completed | failed
logs(execution) -> stream or cursor
cancel(execution)
collect(execution) -> build outputs and result metadata
```

The precise Rust API may evolve, but backend implementations should receive a normalized build request rather than protocol or SSH session objects.

A conceptual request contains:

```text
BuildRequest
├── request ID
├── derivation path
├── target Nix system
├── required system features
├── build options
├── input closure references
├── requester identity
└── cancellation state
```

A conceptual result contains:

```text
BuildOutcome
├── terminal status
├── output paths and metadata
├── build log reference
├── backend and executor identity
├── timing information
└── infrastructure/build failure classification
```

Backend names and APIs should describe domain behavior. Avoid implementation-history names such as `NewBackend`, `LegacyExecutor`, or `UnifiedRunner`.

## Backends

### Local backend

The local backend executes the derivation using a local Nix daemon or store.

Primary uses:

- Development.
- Protocol integration tests.
- Small installations.
- A simple reference implementation for the backend contract.

It should invoke Nix with structured arguments or a library API. It must not interpolate untrusted derivation paths into a shell command.

### Static SSH backend

The SSH backend delegates to a persistent Nix machine.

The machine may expose the ordinary Nix worker protocol over a restricted SSH account. Telchar centrally chooses the machine and transfers required paths.

The backend configuration should describe capabilities:

```text
name
address
systems
supported features
maximum jobs
administrative priority
```

A static SSH backend is useful for existing hardware, specialized machines, Darwin builders, or resources not managed by a cluster scheduler.

SSH must remain restricted to Nix build service behavior. The backend must not provide a general-purpose shell to Telchar credentials.

### Nomad batch backend

The Nomad backend creates one batch execution per derivation.

```text
Telchar request
      │
      ▼
Nomad batch job
      │
      ├── architecture and feature constraints
      ├── explicit CPU, memory, and disk resources where known
      ├── request-scoped credentials
      ├── input staging
      ├── one derivation realization
      ├── output collection
      └── terminal report
```

Nomad owns:

- Node selection.
- Resource placement.
- Pending allocation state.
- Allocation lifecycle.
- Runtime logs.
- Node draining.

An external autoscaler may inspect pending allocations and cluster metrics to adjust the underlying compute fleet. Telchar neither provisions nor identifies individual Nomad clients.

One-shot batch execution has an important lifecycle advantage: build lifetime and allocation lifetime are aligned. Normal node draining can stop new allocations, wait for running builds, and then remove capacity without inventing an SSH-session drain protocol.

The executor should receive a request ID or authenticated request descriptor, not a shell command assembled from user-controlled text.

### Future backends

Possible future backends include:

- Kubernetes Jobs.
- A cloud batch service.
- A dedicated worker protocol.
- A provider that reserves ephemeral machines.

These are not initial requirements. Their possibility validates the backend boundary; it does not justify implementing speculative abstractions.

## Input and output movement

Execution is only useful if every backend can obtain the exact derivation and input closure and return valid outputs.

The baseline transfer path is always through Telchar's gateway store:

```text
Client -> Telchar store -> executor
Executor -> Telchar store -> client
```

This path must work without a shared cache.

A backend may first substitute paths from its normal configured caches, then obtain any missing private paths from Telchar.

After execution:

1. The executor reports expected outputs and Nix result metadata.
2. Telchar imports outputs into its real Nix store.
3. Telchar validates NAR and path metadata, references, expected output names and paths, content-addressed hashes and realisations where applicable, and the supported `BuildResult` fields.
4. Telchar returns a successful build result to the client.
5. Optional cache publication occurs according to policy.

Telchar must not report success merely because a backend process exited successfully. Required outputs must be present and valid at the gateway boundary. For classic input-addressed derivations, this validates store consistency but does not cryptographically prove provenance; executor integrity remains trusted.

## Optional binary-cache integration

A binary cache is an optimization layer, not a correctness dependency.

The cache may be used at three points:

```text
Client-side substitution
        │
        ▼
Telchar read-through substitution
        │
        ▼
Executor input substitution
        │
        ▼
Optional output publication
```

### Client-side cache

Clients may already use public or organizational substituters. A cache hit prevents the request from reaching Telchar. Telchar does not need to control this configuration.

### Gateway read-through cache

Before dispatch, Telchar may check configured substituters for expected outputs. This can avoid duplicate execution when another client or CI system has recently published the result.

Cache lookup must be bounded by a short timeout and fail open to normal execution. An unavailable optional cache must not make builds unavailable.

### Executor input substitution

Executors may use public or shared caches for common input paths. Missing paths must still be obtainable directly from Telchar.

This preserves support for:

- Local source trees.
- Unpublished derivations.
- Private inputs.
- Deployments without a shared cache.

### Output publication

Successful outputs may be published from Telchar to Attic or another Nix-compatible cache. Publication is optional post-MVP behavior and remains an optimization.

The initial publisher is deliberately small:

1. Import and verify outputs in the gateway store.
2. Commit and return the successful result to the waiting client.
3. If policy allows, invoke one bounded publisher command for the verified output paths.
4. Record bounded telemetry; timeout, failure, shutdown, or restart may lose the publication attempt without changing the build outcome.

The publisher is disabled by default, has bounded concurrency and runtime, performs no internal retry, and creates no durable publication job or independent lease. Normal output retention gives the attempt a finite opportunity to finish. External periodic tooling or an operator may republish missed outputs.

Telchar centralizes cache write credentials. Executors should normally have read access to shared caches and request-scoped access to Telchar inputs, but no organization-wide cache publication credential. Durable publication records, leases, retries, and restart recovery require a separate post-MVP decision backed by observed lost-publication impact.

### Privacy policy

Telchar must not automatically publish every client-uploaded input path.

Inputs may contain:

- Proprietary source code.
- Local worktrees.
- Generated sources.
- Sensitive names or content.
- Material intended only for one build.

A safe publication default is:

```text
client-uploaded inputs: never publish automatically
successful outputs: publish only according to explicit policy
```

The initial shared-store trust domain does not promise confidentiality between authenticated clients. Selected executors receive only the closure required for their request, but all executor hosts remain trusted infrastructure. Even outputs may contain proprietary material, so publication policy must remain configurable.

### Cache implementation boundary

Backend APIs must not mention Attic specifically. Cache lookup and publication are separate responsibilities from execution.

The first implementation may invoke stable external tools such as `nix copy` or `attic push` rather than linking to cache-specific internals. Credentials should use file-based service credentials or another secret-delivery mechanism, never plaintext main configuration.

## Identity and quotas

Source IP is useful audit context and a possible emergency fallback, but it is not a sufficient primary identity for organizational quotas.

Source IP is unstable or shared when users are behind:

- NAT.
- Corporate VPNs.
- Shared office networks.
- IPv6 privacy addressing.
- SSH bastions or proxies.

The normalized identity model separates credential, audit, and policy ownership:

```text
Requester
├── credential ID scoped by authentication authority
├── configured or CA-scoped audit subject
├── explicit quota subject, falling back to credential ID
├── certificate key ID and principals, when present
├── source IP audit metadata
└── configured group or policy mapping
```

A key fingerprint identifies a credential or device, not necessarily a person. Multiple keys must not silently bypass an intended person or team quota; deployments needing that behavior must configure their keys and certificates to one quota subject. At work, each engineer or device should receive an individual credential while configured mappings retain stable attribution and quota ownership.

Initial quotas should remain simple:

```text
global max queued
global max running
per-identity max queued
per-identity max running
```

Group quotas, weighted priorities, and reservations can be added when real usage demonstrates a need.

## Scheduling and fairness

A shared build service must prevent one client from consuming every available slot.

The first scheduler should support:

- Separate queues or accounting per requester identity.
- Round-robin selection among identities with runnable work.
- Per-identity running limits.
- Global running limits.
- Administrative priority classes where explicitly configured.
- Backend compatibility by system and features.

Potential later policies include:

- Team quotas.
- CI versus interactive priorities.
- Cost-aware backend preference.
- Affinity toward warm static builders.
- Derivation deduplication.
- Per-project limits.

These should follow observed requirements. Telchar should not begin as a research scheduler.

## Duplicate requests

Multiple clients may request the same derivation concurrently.

A future scheduler may coalesce equivalent requests into one execution and attach multiple interested clients. Correct equivalence must account for Nix semantics, build mode, and relevant options; it must not be guessed from a package name.

The first version may defer active deduplication. Store checks should still prevent unnecessary execution after one result has completed and entered the gateway store.

## Cancellation

Client disconnect and build cancellation are not always equivalent.

A disconnect may mean:

- The user cancelled.
- The network failed.
- SSH restarted.
- The client daemon restarted.
- Another interested client still wants the same result.

A conservative initial policy is:

- Cancel queued work that has no remaining attached requester.
- Allow already running work to finish and populate the gateway store.
- Record that the original request detached.
- Support explicit backend cancellation for administrative use and future reference-counted requests.

Immediate cancellation of every running build on transport loss wastes work and creates difficult result-transfer races.

## Failure classification and retries

Telchar should distinguish at least:

- Build failure: the derivation ran and failed.
- Infrastructure failure: allocation, machine, network, or executor failed.
- Admission failure: quota or policy rejected the request.
- Input failure: required paths could not be staged or verified.
- Output failure: outputs could not be collected or verified.
- Cancellation.
- Internal gateway failure.

Automatic retries should initially be limited to failures known to be infrastructure-related and safe to retry. A failed derivation must not be blindly retried as if a different machine will fix it.

Retries must be bounded and visible in execution history.

## Security model

Nix derivations are executable, potentially hostile code. Organizational deployments must treat build executors as an untrusted workload environment even when all engineers are trusted people.

### Gateway

The gateway should:

- Run with the minimum privileges needed for its Nix store and SSH service.
- Expose only the restricted Nix protocol endpoint.
- Validate store paths and protocol limits.
- Bound uploads, queues, logs, and request counts.
- Keep cache publication credentials away from executors.
- Record requester and backend attribution.
- Avoid shell interpolation.

### Executors

Executors should:

- Enable Nix sandboxing.
- Receive no production credentials.
- Have constrained network access.
- Be isolated from sensitive workloads.
- Use request-scoped credentials for gateway access.
- Have bounded CPU, memory, disk, and runtime where the backend supports them.
- Be disposable where practical.

A Nomad deployment should use a dedicated node pool or equivalently strong isolation. Scheduling arbitrary derivations beside secrets-bearing production workloads is unsafe.

### SSH

SSH access must prohibit:

- Interactive shells.
- PTYs.
- Agent forwarding.
- TCP forwarding.
- X11 forwarding.
- Arbitrary commands.

SSH host certificates and client certificates are supported deployment choices, not core project dependencies.

### Multi-tenancy

A first public release should describe its trust assumptions explicitly. Authentication and quotas do not alone provide safe hostile multi-tenancy. Store visibility, output publication, logs, backend isolation, and network policy all affect tenant separation.

## Autoscaling boundary

Telchar produces demand; infrastructure reacts to it.

For a Nomad backend:

```text
Telchar submits constrained batch job
        │
        ▼
Nomad places or leaves allocation pending
        │
        ▼
External autoscaler observes pending demand
        │
        ▼
Infrastructure adds eligible clients
        │
        ▼
Nomad runs allocation
```

Scale-down is likewise external:

```text
External autoscaler selects idle capacity
        │
        ▼
Scheduler drains node
        │
        ▼
No new batch allocations are placed
        │
        ▼
Existing allocations finish
        │
        ▼
Infrastructure removes node
```

One derivation per batch allocation gives the infrastructure scheduler a concrete unit to place, observe, drain, and terminate. Drain deadlines must be configured so ordinary scale-down does not terminate accepted work. Telchar should expose queue and execution metrics but must not embed provider-specific scaling policy.

Nomad job restart and reschedule policies must be explicit. Telchar remains the owner of retry accounting and must reconcile every allocation belonging to an attempt before deciding that another attempt is safe.

## Observability

Observability is a bootstrap requirement, not a later operational feature. Telchar uses the Rust `tracing` ecosystem for application instrumentation and OpenTelemetry for collection and export. Every application component must emit correlated telemetry from its first implementation rather than adding instrumentation after behavior is complete.

The service supports OTLP export for all three signals:

- Structured logs.
- Metrics.
- Distributed traces.

The initial exporter supports OTLP over gRPC. Endpoint, transport security, headers or credential references, batching, queue bounds, export timeout, resource attributes, and enablement are configuration. OTLP exporter failure must not crash or block the build gateway indefinitely; failures are bounded, locally observable, and accounted for without recursive telemetry loops. A local human-readable `tracing` formatter may be enabled independently for development and emergency diagnosis.

Telchar does not build a second logging or metrics framework beside `tracing` and OpenTelemetry. Code uses `tracing` spans and events for structured execution context and the OpenTelemetry metrics API for instruments. Exporter wiring remains in the Telchar service crate; `nix-worker-protocol` may depend on `tracing` for spans and events but must not configure exporters or depend on the OpenTelemetry SDK.

All long-lived and boundary-crossing operations require spans or metrics as appropriate, including:

- SSH connection and protocol session lifecycle.
- Worker-protocol handshake, operation dispatch, framing failure, and bounded transfer.
- Admission decisions and quota rejection.
- Queue selection and backend ranking.
- Request, attachment, and execution-attempt state transitions.
- PostgreSQL state operations and recovery reconciliation.
- Input staging, execution, log collection, output verification, and delivery.
- Static SSH and Nomad backend submission, polling, cancellation, and collection.
- Cache lookup and publication.
- Administrative actions and shutdown.

Useful metrics include:

- Accepted, rejected, queued, dispatching, backend-pending, running, collecting, completed, and failed requests.
- Queue wait duration.
- Execution duration.
- Input staging and output collection duration.
- Bytes transferred by direction.
- Cache hit, miss, timeout, and publication outcomes.
- Running requests by backend, system, and feature class.
- Backend submission and infrastructure failures.
- Per-identity quota rejections without exposing high-cardinality raw identity labels.
- Detached and cancelled requests.
- Retry counts and reasons.
- Telemetry queue saturation, dropped data, and exporter failures.

Every request has a stable request ID and every execution has stable attempt and backend execution IDs. These identifiers are recorded as bounded span/event fields and propagated into:

- Gateway spans and logs.
- Scheduler and PostgreSQL state.
- Backend execution metadata.
- Nomad job or allocation metadata.
- SSH execution spans and logs.
- Cache publication work.

Trace context is propagated across Telchar-controlled process and network boundaries where the transport permits authenticated metadata. Request IDs remain domain identifiers and do not replace trace and span IDs. Raw requester identities, credentials, store contents, source names, arbitrary derivation strings, and unbounded error text must not become metric labels. Sensitive fields are redacted consistently from logs, spans, and exporter errors.

Tests use a real OTLP collector fixture or protocol-compatible test collector to prove logs, metrics, and traces can be exported and correlated. Unit tests may use in-memory OpenTelemetry exporters for narrow behavior, but acceptance tests must exercise encoded OTLP traffic. Telemetry assertions capture expected exporter errors so passing test output remains pristine.

An administrative CLI or API should eventually support:

```text
telchar status
telchar queue
telchar jobs
telchar job show <id>
telchar job cancel <id>
telchar backends
telchar backend drain <name>
```

The exact command surface should follow implementation needs.

## Configuration direction

Telchar should use a human-readable configuration format such as TOML.

A possible shape, for illustration only:

```toml
[server]
listen = "0.0.0.0:2222"

[quota.defaults]
max_queued = 20
max_running = 4

[[backends]]
name = "local"
kind = "local"
systems = ["x86_64-linux"]
max_jobs = 2
priority = 10

[[backends]]
name = "arm-workers"
kind = "ssh"
systems = ["aarch64-linux"]
max_jobs = 8
priority = 50

[[backends]]
name = "nomad-linux"
kind = "nomad"
systems = ["x86_64-linux", "aarch64-linux"]
priority = 40

[cache]
substituters = [
  "https://cache.nixos.org"
]
lookup_timeout = "3s"

[cache.publisher]
enabled = false
command = ["attic", "push", "telchar"]
timeout = "2m"
max_concurrent = 2
```

Secrets must be referenced by file or injected through service credentials, not embedded in this file.

Configuration should be validated at startup. Unknown backend kinds, impossible capacities, duplicate names, and unsupported systems should fail with actionable errors.

## Rust implementation

Rust is the preferred implementation language.

It is a good fit for:

- Binary protocol correctness.
- Async SSH and network streams.
- Backpressure-aware NAR transfer.
- Strong internal state models.
- Backend interfaces.
- Long-running daemon reliability.
- Static or low-dependency deployment artifacts.
- Property and fuzz testing of protocol parsers.

A likely async runtime is Tokio. Exact libraries should be selected after focused prototypes, especially for:

- SSH server behavior.
- Bounded worker-protocol I/O.
- NAR parsing and streaming.
- Nix store interaction.
- Nomad API access.
- Durable state.

The project should avoid committing to a broad framework before proving the worker-protocol and store vertical slice.

## Workspace and internal modules

The repository begins as a Cargo workspace with a reusable protocol library and the Telchar service:

```text
Cargo.toml
crates/
├── nix-worker-protocol/
│   ├── Cargo.toml
│   ├── src/
│   │   ├── framing.rs
│   │   ├── handshake.rs
│   │   ├── operation.rs
│   │   ├── message.rs
│   │   ├── activity.rs
│   │   ├── error.rs
│   │   └── result.rs
│   ├── tests/
│   └── fuzz/
└── telchar/
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── config.rs
        ├── identity.rs
        ├── protocol_gateway.rs
        ├── store/
        ├── admission/
        ├── scheduler/
        ├── execution/
        ├── backend/
        │   ├── local.rs
        │   ├── ssh.rs
        │   └── nomad.rs
        ├── cache/
        ├── metrics.rs
        └── state.rs
```

The exact module files may evolve with evidence. The crate boundary is required: `nix-worker-protocol` contains reusable wire concepts, while `telchar` contains service and domain behavior.

## Delivery phases

### Phase 0: reproducible baseline and protocol spike

Goal: prove the pinned stock Nix client can communicate with Telchar and establish the compatibility boundary.

- Pin Rust, Nix, and NixOS test inputs in a flake.
- Establish pristine format, lint, unit-test, and real-Nix integration commands.
- Record the initial compatibility matrix.
- Capture the operation sequence used by real remote builds in relevant trust modes.
- Complete Nix worker-protocol handshake over direct stdio.
- Define required, optional, and deterministically rejected operations.
- Establish the `nix-worker-protocol` workspace crate and its dependency boundary.
- Implement protocol behavior independently from captured traffic, primary Nix references, and compatibility tests.
- Record Rio source revision and licensing discrepancy for reference provenance; do not copy or translate Rio implementation in the initial crate.
- Prove the same supported session over restricted OpenSSH `ssh-ng://` ingress.
- Prove trusted identity handoff and negative SSH restrictions.

No scheduler, Nomad, quotas, or cache publication yet.

### Phase 1: local vertical slice

Goal: complete a real derivation through Telchar.

- Gateway-local Nix store.
- Local backend.
- Real input import.
- One derivation execution.
- Log forwarding.
- Output verification and return.
- Real multi-machine `nixosTest` integration test built on the shared harness.

Success criterion:

```text
stock Nix client -> Telchar -> local backend -> output returned
```

### Phase 2: durable request and attempt state

- Single-active daemon with PostgreSQL migrations.
- Domain-specific state operations with explicit transaction ownership.
- Explicit request, attachment, attempt, and outcome states.
- Transactional dispatch and backend idempotency keys.
- Restart recovery and ambiguous-submission reconciliation.
- Gateway-store leases and safe retention cleanup.

### Phase 3: identity, admission, scheduling, and administration

- Trusted requester identity propagation.
- Global transfer bounds and per-quota-subject retained-byte limits.
- Global and per-quota-subject build limits.
- Deterministic fair queue with stable tie-breaking.
- Administrative status and cancellation.
- Structured metrics and audit records.

### Phase 4: remote execution contract

- Request-scoped input and output authorization.
- Backend idempotency, reconciliation, logs, cancellation, and collection semantics.
- Faithful supported `BuildResult` mapping.
- Reusable backend conformance tests.

### Phase 5: static SSH backend

- Capability configuration.
- Input staging.
- Restricted SSH execution.
- Output collection.
- Infrastructure/build failure distinction.
- End-to-end test with a real SSH builder VM.

### Phase 6: Nomad batch backend

- One batch job per derivation.
- System and feature constraints.
- Request-scoped executor credentials.
- Pending/running/completed reconciliation.
- Allocation log integration.
- Cancellation.
- End-to-end test against a local Nomad development cluster or isolated test environment.

### Optional post-MVP cache integration

- Read-through substituters.
- Executor substitution.
- Direct Telchar fallback for missing private inputs.
- Bounded best-effort publication of verified outputs without durable jobs or retries.
- Cache outage and publisher-failure tests proving builds remain functional.
- A separate measured-need decision before durable publication state, leases, retry, or restart recovery.

### Post-MVP organizational hardening

- Group policy.
- Priority classes.
- Stronger isolation guidance.
- Audit exports.
- A separately reviewed active/passive high-availability architecture if required; active/active remains out of scope.
- Duplicate request coalescing if justified.
- Broader compatibility and load testing.

## Testing strategy

Telchar should use test-driven development for protocol features and bug fixes.

### Unit tests

- Identity normalization.
- Quota decisions.
- Fair queue behavior.
- Backend capability filtering.
- Retry classification.
- Configuration validation.
- Protocol framing and version handling.

### Property and fuzz tests

Protocol and NAR handling are suitable for:

- Truncated input.
- Oversized lengths.
- Unknown operation codes.
- Invalid store paths.
- Malformed structured errors.
- Arbitrary byte streams.
- Version-boundary behavior.

### Integration tests

Use real components rather than mocks for core correctness:

- Real stock Nix client.
- Real Nix stores.
- Real SSH transport where SSH behavior is under test.
- Real local execution.
- Real remote SSH builder.
- Real Nomad development agent for Nomad backend tests.
- Optional real binary-cache fixture where practical.

The authoritative whole-system harness is a reusable multi-machine `nixosTest` topology exported by the flake. It starts with separate stock-client and gateway machines plus an OTLP collector, shared readiness and artifact helpers, and packaged Telchar services. PostgreSQL, remote SSH builders, Nomad, and cache fixtures extend this harness as their phases arrive rather than creating unrelated orchestration systems. Specialized tests may define additional machines, but reuse service modules, networking, startup, cleanup, telemetry collection, and failure-artifact conventions.

The harness must retain bounded redacted journals, driver output, machine state, and OTLP records on failure. Successful runs clean temporary state and keep output pristine. Direct component tests remain appropriate for fast protocol and state behavior; `nixosTest` is the authoritative end-to-end acceptance boundary.

The primary acceptance test should prove that the client cannot build locally, submits work to Telchar, and receives a verified output produced by the selected backend.

Expected error logs should be captured and asserted so passing test output remains clean.

## Inspirations and related work

Telchar builds on lessons from existing Nix build systems rather than pretending the problem is unexplored.

### Nix remote builders

Nix's worker protocol and `ssh-ng://` transport provide the compatibility contract. Telchar deliberately preserves this client experience while centralizing scheduling behind one endpoint.

### rio-build

[rio-build](https://github.com/lovesegfault/rio-build) is the strongest architectural inspiration.

Relevant ideas include:

- Implementing the standard Nix worker protocol at a gateway.
- Keeping clients unmodified.
- Treating each derivation as a schedulable execution unit.
- Using one-shot executors.
- Separating gateway, scheduler, store, and executor responsibilities.
- Central scheduling and observability.
- Drain-aware lifecycle management.

Rio-build is Kubernetes-native and includes a sophisticated DAG scheduler, chunked CAS, FUSE-backed stores, controller, and autoscaling design. Telchar intentionally starts with a smaller scope:

- Standard remote-builder mode rather than full remote-store DAG scheduling.
- A normal gateway Nix store rather than a custom CAS.
- Pluggable backends rather than a Kubernetes-only executor controller.
- External infrastructure autoscaling.

Rio-build is archived, so Telchar will not depend on it as an active upstream project. Its `rio-nix` crate is deliberately isolated from other `rio-*` crates and includes worker-protocol, ATerm, NAR, store-path, build-result, and structured-error parsing plus fuzz targets. The repository's root license and Rust package metadata appear inconsistent.

The initial `nix-worker-protocol` implementation does not copy or translate Rio source. Rio remains architectural reference material for identifying concepts, edge cases, and useful categories of tests. Wire behavior is established independently from captured stock-Nix traffic, primary Nix source and documentation, and compatibility tests. A later source import would require a separate reviewed decision and resolved provenance; creation of a reusable crate is not permission to absorb Rio wholesale.

### Yensid

[Yensid](https://github.com/garnix-io/yensid) demonstrates that a stable SSH endpoint can hide a pool of Nix builders and improve global load balancing without client changes.

Relevant lessons:

- One stable remote-builder endpoint.
- Layer-4 transparency.
- Central backend choice.
- SSH certificate support for interchangeable hosts.

Yensid uses HAProxy and static backend configuration, with custom Lua as an extension seam. Telchar moves scheduling above the TCP-connection layer so it can apply quotas, understand derivation lifecycle, and dispatch one-shot jobs.

### Trébuchet

[Trébuchet](https://github.com/Mic92/tribuchet) demonstrates a central hub with dynamic workers, capability matching, draining, and remote execution. It also highlights the operational benefits of workers dialing a scheduler and of separating build execution from classic SSH builder topology.

Trébuchet currently relies on Nix's experimental external-builders feature, while Telchar prioritizes compatibility with stock remote-builder clients.

### Guardian Project nix-builder-autoscaler

The Guardian Project's `nix-builder-autoscaler` demonstrates explicit slot reservation, EC2 lifecycle states, readiness, draining, and termination for elastic Nix builders.

Its Buildbot, Tailscale, and HAProxy integration is not Telchar's architecture, but its state-machine treatment of infrastructure failures and draining is useful prior art.

### Hydra and hydra-provisioner

Hydra and `hydra-provisioner` demonstrate queue-driven build-machine provisioning by system and required features. Telchar does not adopt Hydra's CI/jobset model, but retains the lesson that scaling signals should come from queued build demand rather than raw proxy connection counts.

## Open questions and prototype gates

The following remain bounded research tasks. Each must produce recorded evidence or an architecture decision before dependent implementation begins:

1. Which exact worker operations and protocol versions are exercised by the flake-pinned Nix client in trusted, untrusted, classic input-addressed, and content-addressed cases?
2. Can OpenSSH expose sufficient authenticated public-key and certificate metadata to the forced-command frontend without trusting client-controlled environment data?
3. Which Rio architecture lessons and test categories should inform the independent `nix-worker-protocol` implementation without copying or translating source?
4. Does the dedicated system-store topology satisfy protocol, privilege, fixture-reset, and GC-lease requirements, or is an alternate store root necessary?
5. What request-scoped transfer and authorization protocol lets remote executors obtain exactly the required private closure and return only authorized outputs?
6. What normalized result schema preserves every `BuildResult` field required by the initial compatibility matrix?
7. Which precise failure transitions permit bounded retry after proving the previous attempt inactive or fenced?
8. What bounded log transport and retention policy handles slow clients, disconnects, and backend reconnects? First-release client reattachment may be explicitly unsupported.
9. What minimum Nomad task-driver, sandbox, filesystem, network, resource, secret, cleanup, drain, restart, and reschedule contract is safe enough to support?
10. Should cache publication first use `nix copy`, an Attic CLI, or a native API?

These questions do not justify speculative abstractions. Protocol, identity, store, state, and remote-transfer gates must close before backend breadth begins.

## Initial project charter

> Telchar accepts standard Nix remote-build requests over SSH and schedules each derivation onto a compatible execution backend. It owns protocol compatibility, admission, queueing, quotas, input and output transfer, execution tracking, and result reporting. Backends own execution. Caches are optional optimizations. Machine provisioning and autoscaling remain external concerns.
