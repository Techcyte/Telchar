# Local executor service

## Status

Accepted for the Gate 4 local backend.

## Context

Backend-pending and running attempts must survive Telchar daemon restart without duplicate execution. The existing in-process local executor cannot provide that contract because its worker thread or child process is owned by the daemon process and it exposes no durable submission identity or status lookup.

## Decision

The local backend is a separately running `telchar executor` service. Telchar daemon and local executor have independent process lifetimes. Restarting or fencing the daemon must not terminate accepted executor work.

The executor owns a PostgreSQL-backed execution registry. The registry is authoritative for local-backend submission identity and observed execution state. Telchar's execution-attempt rows remain authoritative for gateway request lifecycle. Neither table substitutes for the other.

Each submission carries deployment-generated values already persisted before contact with the executor:

- attempt ID;
- idempotency key;
- backend execution ID;
- bounded execution specification.

The backend execution ID is stable and derived by Telchar from the attempt identity before submission. Client bytes cannot select it.

The registry has one immutable row per backend execution ID and one unique row per idempotency key. An exact repeated submission returns the existing execution. Reusing either identity with different immutable execution fields rejects. Submission commits before execution begins.

Registry states are:

```text
accepted
running
succeeded
failed
cancelled
```

Terminal states are immutable. State timestamps are monotonic. Each terminal transition atomically inserts one immutable `local_backend_execution_results` row and advances the registry row from `running` to its terminal state. The result row stores a closed classification and bounded object-valued metadata. Successful metadata contains only the build status and typed output name/path pairs; failed and cancelled metadata contains no raw diagnostics or logs. An identical repeated terminal write returns the existing result, while changed terminal state, classification, or metadata conflicts. A service restart reconstructs accepted and running registry rows before accepting new submissions. Recovery never creates a second row or changes submission identity.

## Local protocol

The daemon connects through one deployment-configured absolute Unix socket path. The executor verifies the peer UID. Filesystem mode and peer credentials are both required; pathname permissions alone are insufficient.

The protocol is length-prefixed and versioned. Requests are typed:

```text
submit
status
```

Responses are typed and bounded. Unknown versions, unknown operations, trailing fields, oversized frames, malformed values, timeouts, peer close, and inconsistent duplicate submissions fail closed and invalidate the connection.

The protocol carries no client-selected helper path, store endpoint, trust policy, retry policy, timeout policy, registry endpoint, or process-ownership policy.

The initial implementation permits one request per connection. Request and response frames are bounded to 1 MiB. Executor socket reads and writes have a fixed 30-second timeout. Execution logs and NAR bytes are not retained in the registry protocol.

## Execution ownership

After durable registry insertion, the executor owns execution through terminal registry state. It uses deployment-configured gateway-store access and the existing typed Rust worker-protocol executor. Production does not use PATH discovery, shell commands, host-store fallback, or client-selected endpoints.

Loss of the daemon connection does not cancel execution. Executor shutdown stops admission, preserves durable nonterminal rows, terminates owned workers only during explicit service shutdown, and reconciles those rows on restart before readiness.

The initial local service remains single-active per PostgreSQL deployment using a dedicated advisory-lock connection distinct from the Telchar daemon ownership key. Contention fails startup. Ownership loss permanently fences executor admission and exits.

## Reconciliation

For backend-pending attempts, Telchar queries status using the persisted backend execution ID and idempotency key. A matching registry row advances or preserves the attempt according to observed executor state. Absence or identity mismatch is an explicit reconciliation result; Telchar does not resubmit automatically.

For pre-ID ambiguous dispatching attempts, the executor may be queried by idempotency key. T110 keeps these attempts fenced until authoritative lookup decides whether a registry row exists. Retry policy remains outside this ADR.

## Telemetry

Telemetry may include operation, bounded state, result class, and bounded counts. It must not expose attempt IDs, request IDs, backend execution IDs, idempotency keys, derivations, store paths, outputs, socket paths, database details, helper paths, logs, arguments, environment contents, or executor diagnostics.

## Consequences

The local backend gains a real independently durable execution boundary suitable for restart reconciliation tests. Deployment now owns two long-running processes and two distinct singleton locks. T111 and T112 depend on this service and registry rather than pretending PostgreSQL attempt state proves backend existence.
