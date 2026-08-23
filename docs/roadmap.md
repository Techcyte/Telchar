# Roadmap

This is a ranked list, not a release promise. New work should solve a concrete operational problem without turning Telchar into a CI platform, infrastructure provisioner, cache service, or log product.

## Near term

### Broaden Nix compatibility

Extend the pinned Lix package beyond the local backend and test additional pinned Nix and Lix releases. Stock Nix already has local, OCI, static SSH, and Nomad gateway coverage; keep every client release and backend combination as separate executable evidence.

### Archive logs locally

Add an optional compressed local spool keyed by execution identity, with byte limits, retention, cleanup, and restrictive permissions. Keep log bodies out of PostgreSQL and leave external upload to operator tooling.

## Later

### Active/passive availability

A standby requires leadership epochs, dispatch fencing, callback routing, recovery handoff, and shared or replicated gateway-store authority. Running two current daemons is not high availability. A durable gateway Nix sidecar can provide shared placement-independent store authority, but does not by itself provide daemon leadership or callback failover.

### Narrow pre-execution retries

Consider retries only for failures where Telchar can prove backend execution never started. Transport failure alone is not enough. Never add blind resubmission or a generic retry count.

### Durable client reattachment

Resuming an exact disconnected session needs authentication-bound resume authority, durable cursors, bounded event storage, expiry, and cross-restart semantics. Equivalent requests can already join or reuse shared work.

### Administrative cancellation

Bounded read-only operator inspection already exists. Define who may cancel shared work, follower and owner authority, collection races, audit records, unsupported backend behavior, and replacement semantics before adding mutation commands.

### Floating content-addressed derivations

Require real fixtures and primary Nix evidence for identity, realization, transfer, validation, and result semantics. Fixed-output support does not imply floating content-addressed behavior.

### Reproducible-build provenance

Independent rebuilds or signed provenance need an explicit trust model, quorum or policy rules, key custody, replay protection, retention, and disagreement handling. This is separate from gateway output validation.

### Additional backends

Add Kubernetes, cloud batch, or another scheduler only for a real fleet. Preserve exact persisted execution identity, operator-owned credentials, bounded control-plane behavior, and exact-target recovery.

### Nomad hardware device reservations

Add operator-defined, bounded Nomad device requests only when a real hardware workload requires them. Resource-profile constraints may select a GPU-capable node class, but placement does not reserve exclusive hardware. GPU and other device support must validate allowlisted device names and counts, render Nomad task `Devices` resources, expose reservation telemetry without workload-derived guesses, and include executable scheduling and oversubscription evidence.

### Soak and load qualification

Measure queue depth, duplicate fan-in, transfer throughput, callback concurrency, PostgreSQL pressure, restart storms, retention cleanup, and multi-day execution. Keep safety limits intact while tuning. The in-process suite proves 1,000 simultaneous equivalent requests coalesce into one execution, but does not qualify 1,000 unique PostgreSQL-backed client sessions or 700 concurrent backend executions.

### High-concurrency client runtime

Re-evaluate the blocking process-and-thread-per-client model if deployment evidence approaches 1,000 attached clients. An async runtime may reduce idle session, follower, queue, and permit-wait costs, while blocking Nix store, OpenSSH, and persistence operations remain behind bounded workers. Require measured RSS, thread/process counts, file descriptors, PostgreSQL connection pressure, scheduler wakeups, and shutdown behavior before choosing a runtime migration. Preserve authenticated OpenSSH ingress, durable shared-build authority, backend permits, and exact-target recovery.

### Extract `nix-worker-protocol`

Move the crate only when another consumer or a separate release cadence justifies it. Preserve protocol evidence, fixtures, fuzzing, bounds, and the dependency direction away from Telchar services.

### Hostile multi-tenant isolation

This requires separate stores or namespaces, path authorization, cache and log isolation, backend isolation, recovery isolation, and side-channel review. It is not an incremental database change.

## Permanent exclusions

Unless Telchar's product boundary is explicitly replaced, do not add:

- active/active scheduling;
- generic provider provisioning;
- a Telchar-owned binary cache;
- Redis or object-storage log products;
- alternative durable databases;
- generic automatic retries;
- interactive shells or arbitrary command scheduling;
- native TLS termination.

Public HTTPS and WSS remain the responsibility of an operator-managed reverse proxy or load balancer.
