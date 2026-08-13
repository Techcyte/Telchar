# Telchar future improvements

This document ranks post-MVP work by expected operational value relative to implementation risk. It is a decision aid, not a promise that every item will be implemented.

Before starting an item, require a concrete use case, define its authority boundaries, and add a focused roadmap task. Preserve Telchar's role as a Nix build gateway rather than expanding it into a CI platform, infrastructure provisioner, cache product, or general observability service.

## Recommended order

```text
1. Expand the Nix compatibility matrix
2. Add read-only operator status tooling
3. Add bounded build-log archival
4. Support fixed-output derivations
5. Export autoscaling demand metrics
6. Add a best-effort external cache-publication hook
7. Design active/passive availability when uptime requires it
8. Add narrowly classified pre-execution retries when failure evidence supports them
```

## 1. Expand the Nix compatibility matrix

**Value:** high  
**Risk:** low

Add real-client compatibility cells for selected stock Nix releases. Treat Lix as a separate compatibility target requiring its own traces and fixtures.

### What is missing today

- Confidence that routine Nix upgrades preserve Telchar behavior.
- Support for machines that cannot remain pinned to the currently verified Nix release.
- Lix client compatibility.
- Early warning when worker-protocol behavior changes.

### Bounded implementation

- Capture real client traces for each candidate release.
- Run existing local, static SSH, and Nomad gateway contracts against each supported cell.
- Record exact supported versions and protocol ranges.
- Widen logical protocol behavior only when primary-source and fixture evidence requires it.

## 2. Add read-only operator status tooling

**Value:** high  
**Risk:** low to medium

Prefer a local CLI or protected Unix-socket interface before any network administration API.

### What is missing today

Operators must combine PostgreSQL queries, system logs, and backend-specific tools to answer:

- Which builds are queued, running, collecting, succeeded, or failed?
- Which backend and execution identity own a build?
- Is a build waiting on subject admission or backend capacity?
- Which recovery monitor adopted an execution?
- Why did an execution fail?
- How much configured backend capacity is occupied?

### Bounded implementation

Start with redacted, bounded, read-only commands such as:

```text
telchar status
telchar build <identity>
telchar backends
telchar doctor
```

Do not expose credentials, complete admitted specifications, unbounded diagnostics, or mutation authority. Administrative cancellation is a separate design.

## 3. Add bounded build-log archival

**Value:** high for operations  
**Risk:** low to medium

Store a bounded compressed local spool keyed by durable execution identity, with explicit retention and cleanup. Allow operator-owned tooling to export it.

### What is missing today

- Complete logs after the requester disconnects.
- Earlier log output for late followers.
- Build diagnostics after daemon restart.
- Reliable evidence for overnight failures and incident review.
- Debugging without reproducing the build.

### Bounded implementation

```text
bounded local zstd spool
→ durable execution and attempt identity
→ byte and lifetime limits
→ deterministic cleanup
→ optional external uploader
```

Do not put build-log bodies in PostgreSQL. Do not add Redis-backed logs, built-in object-storage clients, search, or an observability product.

## 4. Support fixed-output derivations

**Value:** medium to high  
**Risk:** medium

Preserve fixed-output hash authority across admission, identity, persistence, routing, execution, validation, and recovery.

### What is missing today

- Telchar cannot execute fixed-output derivations sent through the gateway.
- Cold stores can reject graphs whose fixed-output inputs have not already been substituted or realized.
- Private or isolated dependency graphs may need operator workarounds.
- Telchar is not yet fully transparent as a general Nix remote builder.

Many ordinary builds still work because common fixed-output inputs are already available through substituters or local stores.

### Required proof

- Admit and persist supported output-hash metadata exactly.
- Include hash authority in shared-build identity.
- Forward it unchanged through local, static SSH, and Nomad backends.
- Validate realized output through Nix.
- Prove correct-hash and wrong-hash behavior with stock-Nix fixtures.

## 5. Export autoscaling demand metrics

**Value:** medium to high for elastic Nomad fleets  
**Risk:** low when metrics-only

Expose bounded telemetry that allows external autoscalers to distinguish Telchar scheduling gates.

### What is missing today

External autoscalers cannot directly distinguish:

- No compatible capacity exists.
- Compatible capacity exists but is occupied.
- Subject admission blocks execution.
- Backend permits block execution.
- Nomad placement is pending.
- A particular system or feature set lacks capacity.

Nomad pending allocations and infrastructure metrics remain usable, but Telchar-specific signals improve reaction time and diagnosis.

### Bounded implementation

Possible metrics:

```text
queued_builds{system,features}
backend_permits_used{backend}
backend_permits_max{backend}
build_wait_seconds{gate}
```

Do not provision infrastructure or embed cloud-provider autoscaling logic in Telchar.

## 6. Add a best-effort external cache-publication hook

**Value:** medium  
**Risk:** low to medium

Invoke an operator-controlled command after exact gateway success. Existing Nix cache tooling owns storage, credentials, and publication semantics.

### What is missing today

- Successful outputs are not automatically published for other machines.
- Reuse weakens after gateway retention expires or garbage collection removes outputs.
- Autoscaled cold workers may repeatedly obtain content through substituters or Telchar transfer.
- Cross-site reuse requires separate operator automation.

Operators can already use Attic, `nix copy`, or post-build hooks, so this is an integration convenience rather than gateway correctness.

### Bounded implementation

- Trigger only after exact output validation and durable success.
- Pass bounded output identities, not credentials.
- Make failure observable without changing build success.
- Keep publication best-effort unless operational evidence justifies a separate durable publication design.

Do not implement cache storage, cache credential brokerage, durable publication retries, or a Telchar cache service.

## 7. Design active/passive availability when required

**Value:** medium for production uptime  
**Risk:** high

Design a fenced standby only when measured downtime or maintenance requirements justify it. Active/active scheduling remains outside the recommended path.

### What is missing today

- Daemon-host loss interrupts new ingress.
- Host maintenance requires service interruption or controlled relocation.
- Recovery depends on restarting the singleton owner with access to PostgreSQL and gateway-store authority.

### Required architecture

- Leadership epochs and old-owner exclusion.
- Backend dispatch fencing.
- Callback routing during ownership changes.
- Shared or replicated gateway-store authority.
- Recovery ownership transfer.
- Network-partition and failure-injection proof.

Running two unfenced daemons is not availability; it is duplicate-execution machinery.

## 8. Add narrowly classified pre-execution retries

**Value:** potentially medium  
**Risk:** high

Implement only after production failure data identifies retryable classes whose non-execution is provable.

### What is missing today

- A transient backend failure terminates the shared attempt.
- A later independent request must initiate replacement.
- Users may need to repeat requests after temporary infrastructure failure.

### Safety boundary

A transport failure does not prove that execution never started. Telchar may be unable to distinguish:

- execution never started;
- execution started and remains active;
- build completed but output return failed;
- output imported but terminal persistence failed;
- backend lookup failed temporarily.

Begin, if at all, with failures proven to occur before backend execution identity or dispatch. Do not add generic retry counts or blind resubmission.

## Later, higher-risk capabilities

These have real value in specific environments but poor value-to-risk ratio for the near-term roadmap.

### Durable client reattachment and historical session resumption

**Missing:** a disconnected requester cannot resume its exact attachment, recover prior logs, and continue receiving the original result stream.

Equivalent later requests can already join or reuse durable execution. Full resumption requires authentication-bound resume authority, durable cursors, bounded event storage, expiry, cleanup, and cross-restart semantics.

### Administrative cancellation API

**Missing:** operators lack a convenient supported command to cancel queued or running shared builds.

A correct design must define who may cancel shared work, follower versus owner authority, output-collection races, durable cancellation state, unsupported backend cancellation, auditing, and replacement behavior. Build read-only status tooling first.

### Content-addressed derivations

**Missing:** compatibility with Nix workflows using content-addressed derivations and broader future Nix behavior.

Classic input-addressed evidence does not establish content-addressed identity, realization, transfer, validation, or result semantics. Require real fixtures and primary-source protocol evidence. Implement fixed-output support first.

### Hostile multi-tenant isolation

**Missing:** safe service for mutually untrusted tenants, confidential store paths, and per-path authorization.

This is a separate security architecture requiring tenant-bound stores or namespaces, cache and log isolation, path authorization, backend isolation, recovery isolation, and side-channel review. Adding an owner column to the current shared-store design would not provide meaningful isolation.

### Reproducible-build consensus and cryptographic provenance

**Missing:** evidence that a classic input-addressed output was independently reproduced or endorsed by a trusted provenance authority.

This requires an explicit trust and disagreement model: independent executors, quorum or policy rules, provenance statement formats, signing-key custody, replay protection, retention, and behavior when valid outputs disagree. Keep this separate from ordinary gateway output validation, which proves store and transport consistency rather than honest execution.

### Additional backend kinds

**Missing:** execution through Kubernetes jobs, cloud batch services, or another infrastructure scheduler.

Add a backend only for a concrete fleet. Preserve operator-owned credentials and provider configuration, exact persisted execution identity, bounded submission and observation, exact-target recovery, no client-selected infrastructure, and no generic provider-provisioning layer. Do not abstract existing backends merely to make a hypothetical provider fit.

### OCI images

**Missing:** a supported OCI distribution for the daemon, frontend, or allocation worker.

An image must contain the same packaged binaries and configuration authority as native deployment, document Nix daemon and store access explicitly, run without Docker-specific application behavior, and prove signal handling, filesystem ownership, protected secret delivery, and upgrades. Publishing an image does not make Telchar a container orchestrator.

### Soak, load, and performance qualification

**Missing:** measured operating envelopes for queue depth, duplicate fan-in, transfer throughput, callback concurrency, PostgreSQL pressure, restart storms, retention cleanup, and multi-day execution.

Build deterministic workloads and failure injection before tuning. Record resource ceilings and latency distributions, validate bounded memory and file-descriptor behavior, and distinguish scheduler admission from backend and Nomad placement delay. Do not weaken safety bounds or add caching solely to improve a synthetic benchmark.

### Extract `nix-worker-protocol` into a separate repository

**Missing:** an independently versioned and reusable home for the protocol crate.

Extract only when another real consumer exists or release cadence requires it. Preserve primary-source traceability, fixtures, fuzz targets, protocol bounds, and the ban on Telchar domain or telemetry-exporter dependencies. Define compatibility and coordinated-release policy before moving history; repository extraction alone is not an architectural improvement.

## Explicit exclusions

Do not pursue these without replacing Telchar's product boundary through an explicit architecture decision:

- active/active scheduling;
- generic provider provisioning;
- a Telchar-owned binary-cache service;
- Redis or object-storage log products;
- alternative durable databases; PostgreSQL remains control-plane authority;
- generic automatic retries;
- interactive shell access;
- arbitrary command scheduling;
- hostile multi-tenancy as an incremental database change.

Public HTTPS or WSS deployment uses an operator-managed reverse proxy or load balancer. Transport termination is not Telchar future work.
