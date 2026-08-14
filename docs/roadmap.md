# Roadmap

This is a ranked list, not a release promise. New work should solve a concrete operational problem without turning Telchar into a CI platform, infrastructure provisioner, cache service, or log product.

## Near term

### Broaden Nix compatibility

Test selected stock Nix releases with the complete local, static SSH, and Nomad gateway flows. Treat Lix as a separate target with its own traces and fixtures.

### Add read-only operator tooling

Provide bounded local commands for queue state, build identity, backend occupancy, recovery state, and configuration diagnosis. Start with a local CLI or Unix socket. Mutation and cancellation need separate authority.

### Archive logs locally

Add an optional compressed local spool keyed by execution identity, with byte limits, retention, cleanup, and restrictive permissions. Keep log bodies out of PostgreSQL and leave external upload to operator tooling.

### Substitute cached outputs before backend assignment

After durable shared-build coalescing and subject admission, let the leader ask the configured gateway Nix daemon to `EnsurePath` each missing expected output before acquiring backend capacity. Nix owns substituters, credentials, trusted keys, signature checks, NAR handling, and gateway-store registration. Telchar must not implement binary-cache protocols or allow client bytes to select cache policy.

Bound substitution concurrency and duration. A complete hit must pass the same output validation and retention path as an executed build, then durably complete the shared build and return a normal successful `BuildResult`. A miss, timeout, or incomplete multi-output hit falls through to ordinary backend execution; invalid imported output or gateway-store corruption fails closed. Require real tests for hits, misses, bad signatures, timeouts, partial multi-output availability, coalesced followers, and restart recovery.

### Support fixed-output derivations

Treat fixed-output derivations as an end-to-end compatibility feature rather than a protocol-parser exception. Build on the gateway substitution path so already-valid and substituter-provided fixed outputs exercise the same bounded leader flow and validation boundary as classic outputs. Confirm flat and recursive hashing semantics against pinned Nix sources and real stock-Nix traces before implementation. Carry typed hash mode, algorithm, and digest authority through admission, shared-build identity, persistence, local and static SSH execution, Nomad job and callback protocols, gateway-store validation, and exact-target recovery.

Deliver support in test-led vertical slices, beginning with local execution and then extending the same authority to static SSH and Nomad. Require real fixtures for correct hashes, incorrect hashes, malformed authority, already-valid outputs, substituter-provided outputs, restart recovery, and shared-build coalescing. Do not advertise support until every configured backend and recovery path validates the admitted content authority.

### Export autoscaling demand metrics

Expose bounded signals that distinguish subject admission, backend permit waits, missing compatible capacity, and Nomad placement delay. Autoscaling logic remains external.

### Add a cache publication hook

After output validation, gateway-store import, and durable build success, optionally invoke one operator-controlled executable with bounded output identities. Pass identities without shell interpolation, bound runtime and output, inherit no client-controlled credentials or policy, and emit telemetry for failures. Publication remains best-effort, has no automatic retry queue, and cannot change the Nix build result.

The hook may run operator tooling such as `nix copy`, Attic, or Cachix, but Telchar does not become a binary-cache service. Gateway lookup remains the earlier Nix-daemon substitution phase; publication is a separate post-success operator action.

## Later

### Active/passive availability

A standby requires leadership epochs, dispatch fencing, callback routing, recovery handoff, and shared or replicated gateway-store authority. Running two current daemons is not high availability.

### Narrow pre-execution retries

Consider retries only for failures where Telchar can prove backend execution never started. Transport failure alone is not enough. Never add blind resubmission or a generic retry count.

### Durable client reattachment

Resuming an exact disconnected session needs authentication-bound resume authority, durable cursors, bounded event storage, expiry, and cross-restart semantics. Equivalent requests can already join or reuse shared work.

### Administrative cancellation

Define who may cancel shared work, follower and owner authority, collection races, audit records, unsupported backend behavior, and replacement semantics. Read-only status should come first.

### Content-addressed derivations

Require real fixtures and primary Nix evidence for identity, realization, transfer, validation, and result semantics. Classic input-addressed support does not imply this behavior.

### Reproducible-build provenance

Independent rebuilds or signed provenance need an explicit trust model, quorum or policy rules, key custody, replay protection, retention, and disagreement handling. This is separate from gateway output validation.

### Additional backends

Add Kubernetes, cloud batch, or another scheduler only for a real fleet. Preserve exact persisted execution identity, operator-owned credentials, bounded control-plane behavior, and exact-target recovery.

### OCI images

Publish images only with proven signal handling, filesystem ownership, secret delivery, Nix daemon access, and upgrade behavior. Images must run the same Telchar binaries and do not create a container-specific product mode.

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
