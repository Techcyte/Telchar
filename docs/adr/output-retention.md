# Verified output retention

## Status

Accepted for T093 implementation.

## Decision

Telchar gives every verified request output a deployment-owned local retention guarantee after the complete output root and output-lease set commits.

The guarantee applies to connected and detached requests. A connected stock Nix client may still need time after `BuildResult` delivery to query and copy an output. A detached request needs the same guarantee so a later client can retrieve completed work.

The configuration is:

```text
TELCHAR_OUTPUT_RETENTION_SECONDS
minimum: 60
maximum: 86,400
default: 3,600
```

Unknown, empty, non-Unicode, non-integer, zero, below-minimum, and above-maximum values fail startup. Client and worker-protocol bytes cannot select or extend the duration.

## Durable deadline

Each request output lease stores an explicit `expires_at` timestamp.

```text
complete output root set created
→ create complete output lease set atomically
→ expires_at = PostgreSQL transaction timestamp + configured duration
→ commit
```

The deadline is immutable. Later configuration changes do not retroactively change existing leases.

Derivation, input, and transfer leases have no expiry deadline. Active and released output leases preserve their deadline as durable lifecycle evidence.

Migration `0002_output_retention` backfills every existing active output lease with:

```text
migration transaction timestamp + 1 hour
```

This backward-compatibility behavior is explicitly approved. Existing released output leases receive the migration transaction timestamp as their historical deadline because they are already cleanup-eligible and must not become active again.

## Expiry transaction

Expiry is a PostgreSQL domain transition, not a filesystem scan.

A bounded operation selects request-owned active output leases whose deadline is at or before an injected `now` value:

```text
ORDER BY lease_id
LIMIT <= 256
FOR UPDATE SKIP LOCKED
```

The operation marks the complete selected set released in one explicit transaction and returns the post-transition rows in deterministic lease-ID order. Commit occurs before any root removal.

```text
select and lock eligible active output leases
→ mark selected leases released
→ commit
→ remove exact Telchar roots named by returned rows
```

A transaction failure leaves leases active and roots intact. A root-removal failure after commit leaves released rows as reconciliation authority and roots intact for a later retry.

## Reconciliation

The single active Telchar daemon performs bounded reconciliation:

1. before readiness during startup;
2. from one synchronous maintenance thread every 60 seconds.

Each pass uses keyset pages of at most 256 rows. `FOR UPDATE SKIP LOCKED` makes the expiry transaction safe against overlapping maintenance calls without turning multi-daemon operation into a supported deployment mode.

Released derivation/input roots and expired output roots use durable released rows as cleanup authority. Root filenames, directory scans, path existence, and cache state are not lifecycle authority.

Unknown, active, non-expired, nested, mismatched, non-symlink, or directory roots fail closed.

## Retrieval and cache publication

Before expiry, a stock Nix client can query and copy the exact output through Telchar using the normal worker operations:

```text
QueryPathInfo
→ NarFromPath
→ verified raw NAR delivery
```

This proof does not require rebuilding and does not require binary-cache publication.

Binary-cache publication is independent:

- publication success does not extend local retention;
- publication failure does not shorten local retention;
- cache publication is not correctness authority for T093;
- T093 does not release roots early after publication.

After expiry and exact root removal, Telchar no longer guarantees local availability. The path may remain in the gateway store until Nix garbage collection. A cache may still provide it if publication completed, but that is a separate contract.

## Resource and security boundary

Users indirectly cause output leases by submitting authenticated builds, but users do not own or control leases. Telchar generates lease IDs, selects exact validated paths, sets deadlines, and performs release.

The one-hour guarantee is not user storage and not a cache quota. Time bounds plus the existing gateway disk-reserve admission limit exposure, but per-requester retained-byte quotas are deferred to quota and fairness work.

Telemetry may expose only bounded low-cardinality fields such as configured duration, operation, result, failure class, and page count. It must not expose lease IDs, request/session/owner IDs, store paths, deadlines, SQL, URLs, credentials, NARs, logs, or helper output.

## Consequences

Verified outputs remain retrievable through requester disconnect and ordinary post-result copying. Retention is finite and deployment-owned. Durable release always precedes root removal. Cache publication remains an optimization and recovery source, not a prerequisite for correctness.
