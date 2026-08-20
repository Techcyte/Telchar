# Singleton ownership lease

## Status

Accepted.

## Context

Telchar must admit durable mutations from one daemon and one local executor per deployment database. PostgreSQL session advisory locks provided immediate exclusion, but a service-mesh proxy could retain the PostgreSQL session after the owning process or node disappeared. Replacement then required manual termination of the orphaned database backend.

TCP keepalive on the application connection cannot guarantee release of a separate proxy-to-PostgreSQL connection. Nomad-specific lifecycle behavior is also not a portable ownership authority.

## Decision

PostgreSQL stores one lease for each ownership kind:

- `daemon`;
- `local-executor`.

An acquisition creates or replaces an expired lease and increments a monotonic generation. The default renewal interval is five seconds and the default lease duration is twenty seconds. Both are configurable through the database service configuration, with a required lease duration of at least three renewal intervals.

Database time determines acquisition, renewal, verification, and expiration. Process clocks do not determine authority.

Connections used by an owner carry its kind, token, and generation as PostgreSQL session settings. Statement-level triggers on Telchar's mutable durable tables verify that the configured generation still owns an unexpired lease. Reads and operator commands do not require ownership settings. Migrations remain separately serialized by their transaction advisory lock.

A replacement may acquire only after expiration. It receives a higher generation. Any surviving prior process is then unable to renew or mutate durable state. Its next renewal fails, causing fail-closed daemon shutdown and IPC socket removal.

## Consequences

Abrupt process, node, proxy, and network loss release ownership within the configured lease duration without manual database-session termination. Graceful shutdown deletes only the caller's matching lease generation.

The takeover bound is the lease duration plus scheduling and startup time. Operators must not configure the duration below three renewal intervals. Lower values trade faster takeover for less tolerance of database latency and scheduling pauses.

Every mutable Telchar table must remain covered by the ownership trigger. A migration that adds a durable mutable table must add the trigger in the same migration.
