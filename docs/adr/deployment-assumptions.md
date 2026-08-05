# Deployment Assumptions

**Status:** Accepted

## Context

Telchar provides a Nix remote-builder gateway. The initial deployment needs explicit operational, persistence, ingress, and trust boundaries so implementation work does not imply unsupported distributed operation or tenant isolation.

## Decision

- Linux-first initial support.
- Telchar operates as a single-active deployment: one daemon owns scheduling, durable state, gateway-store coordination, and backend reconciliation. It acquires one stable PostgreSQL advisory lock on a dedicated lifetime connection before activating service work; contention fails startup and connection loss fences the daemon before bounded exit.
- OpenSSH provides network-facing SSH ingress. A restricted forced command starts one `telchar serve-stdio` frontend per connection. Each frontend communicates with the daemon through authenticated local IPC.
- PostgreSQL is the durable control-plane database. PostgreSQL does not provide multiple active schedulers or Telchar high availability.
- Telchar accesses persistence through domain-specific state operations with explicit transaction ownership. Database interchangeability is not an initial goal.
- TOML is the initial human-readable service configuration format.
- The gateway runs on a dedicated host or VM whose system Nix store is controlled by Telchar and is not shared with unrelated workloads.
- Authenticated clients initially share one mutually trusted store domain. Hostile client multi-tenancy and per-path client authorization are deferred.

## Consequences

Telchar implementation assumes one active daemon and does not treat PostgreSQL as a distributed scheduler, leadership, or failover mechanism. The advisory lock prevents accidental split brain but does not provide automatic failover. Post-MVP active/passive high availability requires a separate design for leadership epochs, protocol-session routing, backend dispatch fencing, gateway-store availability, and failure injection. Active/active scheduling remains out of scope.

The shared store domain is appropriate only for mutually trusted authenticated clients. It is not a security boundary between hostile tenants.

A future database requires separate design for migrations, concurrency, recovery, and integration testing.
