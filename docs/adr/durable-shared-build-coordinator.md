# Durable shared-build coordinator

## Status

Accepted and implemented.

## Context

Telchar exposes one stock-Nix `ssh-ng` endpoint and executes admitted normal-mode derivations through local, static SSH, or Nomad backends. Multiple clients may request the same derivation concurrently. Starting one backend execution per client wastes Nix's stable derivation identity and can create duplicate work on different builders.

The coordinator stays deliberately narrow: durable waiting, subject fairness, bounded execution allocation, attempt history, and exact backend recovery identity. Generic reservations, automatic retries, priorities, billing, and active/active ownership remain outside its scope.

## Decision

Telchar uses a small durable shared-build coordinator.

One equivalent normal-mode derivation has at most one active shared execution. The derivation store path is the primary key and a bounded digest of the admitted execution specification detects inconsistent requests for the same path.

A shared build has five states:

```text
claimed
running
collecting
succeeded
failed
```

The durable shared-build record contains the identity and state needed to coalesce requests, attribute one trusted quota owner, wait in a subject-fair queue, select and identify one backend execution, reconcile after restart, validate expected outputs, and publish one terminal result. Queue position and the last-admitted subject are durable. Priority, billing, generic capacity reservations, and retry policy are not.

The first admitted requester supplies the trusted `quota_subject` that owns the shared execution allocation through terminal completion. Matching requesters cannot replace that owner. One process-local requester becomes the execution leader; matching requesters receive a bounded already-in-progress diagnostic and wait synchronously for the durable shared terminal result. Followers consume transfer limits but no additional queued allocation, active execution allocation, attempt, or backend permit.

Queued leaders are selected round-robin across eligible quota subjects and FIFO within each subject. Per-subject queued and active execution limits are transactional. Client disconnect does not release queue ownership or cancel admitted backend work under the default detach-and-finish policy.

Each admitted execution creates one durable attempt tied to the shared-build identity. The attempt records its ordinal, exact selected backend name and kind, optional external execution ID, running and collecting progress, and one terminal outcome. Attempts provide history and recovery substrate; Telchar does not automatically create another attempt after failure.

Backend selection uses exact system compatibility, required-feature subset compatibility, declaration order, and a bounded per-backend permit. Quota admission occurs before backend permit acquisition, so an admitted build may durably remain `running` while waiting for backend capacity. Different derivations may fan out concurrently when permits allow. Nomad owns cluster placement, pending allocations, resource scheduling, and interaction with infrastructure autoscaling.

Each backend advertises a small typed capability set consumed by the coordinator. Capabilities describe observable control-plane guarantees, not implementation details:

- Execution recovery is either `output-only` or `adoptable`. An adoptable backend supplies a stable external execution ID and can query and resume monitoring that exact execution after coordinator ownership changes. An output-only backend can verify completed outputs but cannot prove that an incomplete execution remains active.
- Cancellation is either `connection-bound` or `explicit`. Explicit cancellation addresses a stable backend execution ID. Connection-bound cancellation cannot be transferred to another coordinator instance.
- Log recovery is either `live-only` or `replayable`. Replayable means a monitor can resume from a bounded backend cursor or durable archive; it does not imply unlimited retention.

System, features, declaration order, and configured permits remain backend target properties rather than capabilities. Multiple connected Telchar clients consuming one live log stream is coordinator fan-out, not a backend capability: the coordinator owns one backend monitor and broadcasts bounded chunks to current local followers. Cross-instance or post-restart log attachment requires `replayable` log recovery and is outside the MVP.

Current backend declarations are:

```text
local:      output-only, connection-bound cancellation, live-only logs
static SSH: output-only, connection-bound cancellation, live-only logs
Nomad:      adoptable, explicit cancellation, live-only logs
```

Nomad log replay may be added later only if its bounded cursor, retention, and redaction semantics are specified and tested.

Telchar performs no automatic retry. A failed shared build is terminal for its current requesters. A later client request may atomically replace the failed record and start a fresh execution after checking the gateway store.

## Restart behavior

Startup first verifies the exact expected output set in the gateway store. Valid outputs complete the shared build and its active attempt regardless of the previous nonterminal state. Active rows created before attempt tracking are backfilled during migration.

Recovery follows the persisted backend identity and advertised execution-recovery capability:

- An `adoptable` backend must query the exact persisted external execution ID. Nomad jobs use a deterministic identity, so Telchar adopts the existing job, resumes monitoring it independently of client attachment, and collects its outputs without blind resubmission.
- An `output-only` backend checks exact verified outputs. Local and static SSH executions cannot be identified or reattached through a fresh connection. If the complete output set does not exist, Telchar marks the shared build failed. A later normal Nix request may start it again.

Capability disagreement between current configuration and a persisted active shared build fails closed. Telchar never upgrades an `output-only` record into an adoptable execution by guessing or resubmitting work.

Startup reconciliation establishes bounded monitors and does not wait indefinitely for active backend jobs before service readiness.

## Logs

Logs are connection-scoped. The execution leader receives live backend logs. Followers and reconnecting clients receive a bounded already-in-progress message but no historical replay. Losing Telchar's process may lose unarchived log history without losing durable shared-build identity or completed outputs.

Historical log archival is not implemented. The preferred extension is a bounded local zstd spool with opaque filenames, restrictive permissions, disk-reserve checks, atomic finalization, and optional external upload. Redis, PostgreSQL log bytes, and a Telchar-owned log service remain out of scope.

## Consequences

The coordinator provides duplicate suppression, durable subject-fair waiting, bounded per-subject execution allocation, attempt history, compatible-backend fan-out, client-independent execution ownership, durable Nomad adoption, and ordinary Nix retry behavior without implementing a general scheduler.

The coordinator preserves clear extension seams:

- Priorities can rank eligible shared builds without changing request equivalence or backend contracts.
- Automatic retries can create later explicit attempt ordinals under a separately approved retry policy without redefining terminal outputs.
- Administrative status and cancellation APIs can address stable shared-build and backend execution identities, gated by advertised cancellation capability.
- Durable log archives or backend cursors can add replayable log recovery without changing live coordinator fan-out.
- Additional backend kinds can join by declaring the same bounded capabilities rather than adding backend-kind conditionals throughout the coordinator.

These extensions require new policy and schema when justified. Telchar does not retain dormant scheduler transitions merely to make those additions appear pre-implemented.
