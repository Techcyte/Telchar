# PostgreSQL singleton ownership

## Status

Accepted and implemented.

## Decision

Each Telchar deployment uses the fixed signed 64-bit PostgreSQL advisory-lock key `0x5445_4c43_4841_5202`. This constant is the ASCII-derived Telchar namespace `TELCHAR` plus ownership-contract version `2`. It is not configurable and is not derived from client, requester, system, database URL, hostname, or other deployment input. Because deployments for different Nix systems use independent PostgreSQL databases, one fixed key enforces one active Telchar daemon per deployment database.

The daemon acquires the lock with `pg_try_advisory_lock` on one dedicated lifetime PostgreSQL connection. Ownership must be acquired before admission, scheduling, reconciliation, administrative mutation, backend submission, or listener readiness. The lock connection performs no ordinary domain transactions. A false result is a deterministic startup refusal; the daemon does not wait, partially activate, or retry.

The lifetime connection is monitored while service work is active. Any database read failure, server termination, socket close, protocol failure, or unexpected lock loss permanently fences that process. Fencing closes admission first and prevents every subsequent scheduling, retry, cancellation, reconciliation mutation, administrative mutation, and backend submission. The process then stops listeners, joins bounded internal work, closes the ownership connection last during graceful shutdown, and exits within the deployment shutdown bound. Existing external backend work remains durable and is reconciled by a later owner.

No reconnect preserves or restores ownership. A process that loses the connection cannot become active again. A replacement process may acquire ownership only after PostgreSQL has released the prior session lock. This contract is single-active process exclusion; it does not provide high availability, leader election, automatic failover, or active/active scheduling.

## Failure and shutdown table

| Event | Required behavior | Operator-visible result |
| --- | --- | --- |
| Contention | Refuse startup before readiness or side effects; do not wait or retry. | Exit failure and `database.singleton_ownership.refused`. |
| Database disconnect | Permanently fence, close admission, prevent new durable or external side effects, then perform bounded exit. | `database.singleton_ownership.lost` with a bounded failure class. |
| Reconnect | Forbidden as ownership continuity. Process remains fenced and exits. | No acquired event after loss. |
| Graceful shutdown | Stop admission and listeners, finish bounded shutdown work, then close the dedicated lock connection last. | One normal shutdown sequence; lock becomes available after connection close. |
| Process crash | PostgreSQL releases the session lock when the connection dies; no process-local cleanup is assumed. | Replacement may acquire only after release is authoritative. |

## Telemetry

Successful acquisition emits `database.singleton_ownership.acquired`. Startup contention emits `database.singleton_ownership.refused`. Lifetime connection loss emits `database.singleton_ownership.lost` before bounded exit. Events may contain only operation, result, and bounded failure class. They must not contain the database URL, credentials, advisory-lock value, SQL, endpoint, hostname, requester data, request or attempt IDs, store paths, or backend identifiers.

## Consequences

The ownership connection is a service-lifetime resource distinct from pooled or per-operation persistence connections. Lock loss is terminal rather than recoverable. Operators provide process restart and PostgreSQL availability; Telchar makes no high-availability claim.
