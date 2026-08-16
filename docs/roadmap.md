# Roadmap

This is a ranked list, not a release promise. New work should solve a concrete operational problem without turning Telchar into a CI platform, infrastructure provisioner, cache service, or log product.

## Near term

### Broaden Nix compatibility

Expand beyond the executable stock Nix 2.34.8 and Lix 2.94.2 local-backend fixtures. Test selected client releases with complete static SSH and Nomad gateway flows; keep each client and release as separate evidence.

### Archive logs locally

Add an optional compressed local spool keyed by execution identity, with byte limits, retention, cleanup, and restrictive permissions. Keep log bodies out of PostgreSQL and leave external upload to operator tooling.

## Later

### Active/passive availability

A standby requires leadership epochs, dispatch fencing, callback routing, recovery handoff, and shared or replicated gateway-store authority. Running two current daemons is not high availability.

### Narrow pre-execution retries

Consider retries only for failures where Telchar can prove backend execution never started. Transport failure alone is not enough. Never add blind resubmission or a generic retry count.

### Durable client reattachment

Resuming an exact disconnected session needs authentication-bound resume authority, durable cursors, bounded event storage, expiry, and cross-restart semantics. Equivalent requests can already join or reuse shared work.

### Administrative cancellation

Define who may cancel shared work, follower and owner authority, collection races, audit records, unsupported backend behavior, and replacement semantics. Read-only status should come first.

### Floating content-addressed derivations

Require real fixtures and primary Nix evidence for identity, realization, transfer, validation, and result semantics. Fixed-output support does not imply floating content-addressed behavior.

### Reproducible-build provenance

Independent rebuilds or signed provenance need an explicit trust model, quorum or policy rules, key custody, replay protection, retention, and disagreement handling. This is separate from gateway output validation.

### Additional backends

Add Kubernetes, cloud batch, or another scheduler only for a real fleet. Preserve exact persisted execution identity, operator-owned credentials, bounded control-plane behavior, and exact-target recovery.

### OCI images

The executable Docker-backed fixtures prove archive loading, declared entrypoints, non-root gateway execution, real Nix-daemon socket access, stock-Nix classic and fixed-output builds, PostgreSQL migration and ownership fencing, retained-result reuse, graceful and crash restart, store interruption, exact-archive redeployment, future-schema rejection, backup restore, and no blind resubmission. Operator-owned secret delivery remains deployment policy. Images run the same Telchar binaries without a container-specific product mode.

### Soak and load qualification

Measure queue depth, duplicate fan-in, transfer throughput, callback concurrency, PostgreSQL pressure, restart storms, retention cleanup, and multi-day execution. Keep safety limits intact while tuning.

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
