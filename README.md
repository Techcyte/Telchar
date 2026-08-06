# Telchar

Telchar is intended to be a self-hosted Nix build gateway. It will present one stable remote-builder endpoint to stock Nix clients, apply admission and scheduling policy, and dispatch submitted build operations to compatible execution backends.

Clients will continue using ordinary Nix commands and remote-builder configuration. They will not need a custom client, a patched Nix installation, or knowledge of the execution fleet.

The implemented boundary currently completes stock-Nix handshake and `SetOptions`, then returns a framed unsupported-operation error. Typed classic-build traffic has been observed byte-transparently through a test-only proxy to a real Nix daemon, but Telchar does not yet execute or return a build. Gateway store, persistence, scheduler, and backend sections below describe target architecture.

```text
Stock Nix client
      │
      │ SSH + Nix worker protocol
      ▼
OpenSSH forced-command frontend
      │
      │ authenticated local IPC
      ▼
Telchar daemon
      ├── protocol gateway
      ├── admission and fair queues
      ├── PostgreSQL execution state
      ├── gateway Nix store
      ├── backend selection and reconciliation
      └── logs, metrics, and traces
             │
             ├── local Nix
             ├── static SSH builder
             ├── Nomad batch job
             └── future backend
```

## Responsibilities

Nix remains responsible for evaluating expressions and deciding which derivations are ready to build. Telchar does not reconstruct a global derivation graph. It schedules only operations that client Nix daemons submit.

Telchar owns:

- Authentication-derived requester identity and audit attribution.
- Admission control, quotas, transfer limits, and bounded ingress credit.
- Fair queueing across quota subjects.
- Capability-aware backend selection.
- Durable request, attachment, execution-attempt, and terminal-outcome state.
- Input staging through a gateway-owned Nix store.
- Backend submission, cancellation, collection, retry classification, and restart reconciliation.
- Output verification and streaming back to the requesting Nix client.
- Administrative events and correlated OpenTelemetry signals.

External infrastructure owns machine provisioning and autoscaling. A binary cache may accelerate transfers, but it is never required for correctness.

## Process topology

The network-facing SSH service is OpenSSH. A restricted forced command starts one `telchar serve-stdio` frontend for each client connection. The frontend owns only the worker-protocol stream and its attachment to a request. It does not run an independent scheduler and does not connect directly to PostgreSQL.

Frontends communicate with one central Telchar daemon over authenticated local IPC. The daemon owns shared policy, durable state, the gateway store, backend lifecycle, and administration.

The initial topology is single-active. Before activating admission, scheduling, reconciliation, or mutating administrative work, the daemon acquires one stable PostgreSQL advisory lock on a dedicated lifetime connection. A second daemon sharing the database fails startup. Loss of the lock connection fences the active daemon: it stops new side effects and exits within a bound so a replacement can perform normal recovery.

The advisory lock prevents accidental split brain. It is not leader election or automatic high availability. Active/passive operation would additionally require leadership epochs, protocol-session routing, dispatch fencing, gateway-store availability, and failure-injection proof. Active/active scheduling is outside the initial architecture.

## Build flow

A submitted build follows this path:

1. OpenSSH authenticates the client and starts the restricted frontend.
2. The frontend forwards authenticated requester metadata and worker bytes to the daemon over one bounded local stream.
3. The daemon validates requester metadata, negotiates the supported Nix worker protocol, and checks system, feature, quota, concurrency, and transfer policy.
4. Required inputs are copied into the gateway store under bounded transfer admission.
5. The request enters a fair queue. Backend capacity is distinct from client ingress credit.
6. The daemon creates a durable execution attempt and backend idempotency key before submission.
7. A compatible backend stages inputs, executes the derivation, and returns logs, status, metadata, and outputs.
8. Telchar imports and verifies outputs in the gateway store before reporting success.
9. Results and logs are relayed through the original worker-protocol session.
10. Store leases retain required inputs and outputs until requests, attachments, and transfers no longer need them.

Protocol sessions, build requests, attachments, attempts, and outcomes are separate records. One request may have multiple attachments and bounded sequential attempts, but only one unfenced active attempt. Terminal attempt history is immutable.

## Nix worker protocol

Reusable wire behavior lives in the `nix-worker-protocol` workspace crate. Its boundary includes:

- Bounded wire primitives and framing.
- Protocol versions and feature negotiation.
- Worker operation codes and typed request and response messages.
- Activity, log, error, and build-result wire representations.
- Compatibility fixtures, property tests, and fuzz targets.

It contains no Telchar identity, scheduler, PostgreSQL, gateway-store, ingress, backend, cache, or service-configuration concepts. Telchar adapts typed protocol messages to those domain operations.

The Nix worker protocol is binary, stateful, versioned, and operation-specific. It has no generic message envelope that allows unknown operations to be skipped safely. Telchar therefore derives accepted boundaries from pinned Nix serializers and real-client fixtures. Payload bodies are streamed with bounded memory; diagnostics retain only approved bounded metadata. Unknown or untyped traffic fails closed.

Protocol support is evidence-based. Successful version negotiation alone is not a compatibility claim. Each supported client, trust mode, and derivation class requires typed coverage and a real-client fixture proving the complete operation, response, callback, upload, and result flow.

The implementation is derived independently from primary Nix source, fixed fixtures, typed captures, and compatibility tests. Other projects may inform architecture and test categories, but their protocol implementations are not copied or mechanically adapted.

## Gateway store and data movement

The daemon owns a persistent gateway Nix store used to receive client inputs, stage backend transfers, verify outputs, serve results, and recover interrupted operations. It is not disposable scratch space.

The gateway store maintains explicit leases tied to requests, attachments, attempts, active transfers, and publication work. Cleanup removes paths only when no live lease or configured retention rule requires them. Nix garbage collection remains the deletion mechanism; Telchar controls when paths become eligible.

Transfers are streaming and bounded. Request admission and transfer admission are separate controls so large imports or downloads cannot exhaust file descriptors, memory, disk bandwidth, or store capacity while nominal request concurrency remains low.

## Scheduling and execution

Scheduling combines per-subject fairness with capability-aware backend selection. A deployment advertises a deliberate envelope of supported systems and features. Unsupported combinations fail promptly instead of waiting indefinitely.

Initial backend types are:

- **Local Nix:** executes through the gateway host's Nix daemon and provides the reference backend contract.
- **Static SSH:** uses a configured remote Nix builder with bounded staging, execution, collection, cancellation, and health behavior.
- **Nomad batch:** submits one job per durable attempt, derives object identity from the attempt ID, reconciles job state, streams logs, and collects outputs. Telchar—not Nomad—owns retry policy.

Backend APIs describe execution lifecycle rather than provider-specific mechanisms. They accept normalized requests and expose capability, submission, observation, cancellation, and collection operations.

Duplicate requests may share completed work or one active execution only when authorization, requested outputs, system, features, protocol semantics, and failure behavior are compatible. Deduplication is an optimization, never a correctness requirement.

## Persistence and recovery

PostgreSQL is the durable control-plane database. Database interchangeability is not an initial goal. Telchar uses transactions, constraints, locking, and `RETURNING` semantics where they improve correctness rather than hiding them behind generic CRUD repositories.

Persistence is exposed through domain operations such as accepting a request, attaching a session, claiming runnable work, creating an attempt, recording submission, transitioning execution state, completing a request, acquiring store leases, and recovering incomplete attempts. Protocol, scheduler, and backend code do not issue arbitrary SQL.

Dispatch state changes and attempt creation are transactional. Recovery reconciles ambiguous backend submission before resubmitting anything. Retries create linked attempts; they do not rewrite terminal history. Automatic retry is conservative and requires an infrastructure-classified failure, proof that the previous attempt is inactive or fenced, and explicit permission from a tested transition policy.

Cancellation distinguishes queued, dispatching, backend-pending, running, and collecting work. A disconnected client does not necessarily cancel shared work: cancellation depends on remaining attachments and administrative policy.

## Identity, trust, and security

OpenSSH supplies authenticated credential information. Telchar normalizes it into separate concepts:

- Credential ID.
- Audit subject.
- Quota subject.
- Certificate issuer and principals, when present.
- Source-address metadata.

Source address is audit context, not primary identity. Authorized keys and OpenSSH user certificates may map into the same policy model without coupling scheduling to a particular certificate issuer.

Authenticated clients initially share one mutually trusted Nix store domain. A client that knows a store path may be able to query or download it. Path opacity is not authorization. Hostile client multi-tenancy, per-tenant stores, and per-path authorization require a different isolation model.

Build payloads are untrusted and must execute in Nix sandboxes. Executor hosts and their Nix daemons are trusted for build-result integrity. Telchar verifies transport and store invariants, expected outputs, NAR metadata, references, and content-addressed hashes where applicable. It cannot prove that a classic input-addressed output was honestly produced; reproducible rebuild or consensus verification is outside scope.

The SSH frontend exposes no shell, PTY, forwarding, agent forwarding, or arbitrary command execution. Executors receive only the credentials and network access required for their assigned work. Organization-wide cache publication credentials remain centralized rather than being distributed to builders.

## Cache integration

Binary-cache support is optional and separated from execution:

- Clients may substitute before contacting Telchar.
- The gateway may perform read-through substitution before scheduling.
- Executors may use configured read-only caches for inputs.
- After a verified successful build, the daemon may invoke one bounded centralized publisher command.

Cache miss, outage, or publication failure never changes build correctness. Verified outputs are committed and returned first. The initial architecture does not require durable publication queues, independent publication leases, automatic publication retries, or restart recovery for publication. Those mechanisms require measured operational need.

Cache visibility must not broaden access beyond deployment policy. Shared cache namespaces are appropriate only within the same trust domain; stronger tenant isolation requires separate namespaces, credentials, and authorization.

## Observability

Rust application code emits structured events through `tracing`. The Telchar service owns OpenTelemetry setup and exports correlated OTLP logs, metrics, and traces. The protocol crate may create instrumentation but does not own exporters.

Telemetry includes request, attachment, attempt, backend, quota-subject, and protocol-session correlation where applicable. Sensitive protocol bodies, derivations, NAR contents, secrets, raw authentication material, and unbounded strings are excluded.

Exporter queues, cardinality, retries, timeouts, and shutdown flushing are bounded. An unavailable collector must not crash Telchar, block startup indefinitely, recurse through exporter errors, or create unbounded memory growth.

Operational signals cover queue depth and age, admission rejection, transfer pressure, backend capacity and health, execution duration, retries, cancellation, recovery, gateway-store usage, cache outcomes, and protocol errors. Autoscalers consume these signals but remain external to Telchar.

## Configuration

Configuration is TOML and is expected to cover:

- Daemon listen and local IPC settings.
- PostgreSQL connection and singleton-lock settings.
- Gateway-store location and retention policy.
- Supported systems and feature envelope.
- Identity mapping, quotas, and concurrency limits.
- Transfer admission and storage limits.
- Backend definitions and capability labels.
- Retry, cancellation, and timeout policy.
- OpenTelemetry export and local logging.
- Optional cache lookup and publication commands.

Secrets belong in files or a secret-delivery mechanism, not inline in the main configuration.

## Non-goals

Telchar does not aim to:

- Evaluate flakes or Nix expressions.
- Replace `nix-daemon` on client machines.
- Reconstruct Nix's derivation-readiness graph.
- Act as a CI orchestrator or receive source-control webhooks.
- Manage jobsets or build matrices.
- Provision or destroy compute instances.
- Replace Nomad, Kubernetes, or another infrastructure scheduler.
- Require or replace a particular binary-cache implementation.
- Implement a new content-addressed object store.
- Schedule arbitrary shell commands.
- Expose interactive builder shells.
- Transparently migrate an in-progress build between executors.
- Provide active/active gateway scheduling in the initial architecture.
- Provide hostile multi-tenant store isolation without separate security design.

## License

Telchar is licensed under the [MIT License](LICENSE).
