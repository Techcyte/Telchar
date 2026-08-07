# Dedicated Gateway-Store Ownership

**Status:** Accepted for the initial Gate 3 implementation boundary

## Scope and trust boundary

The gateway store is a dedicated Nix store owned by the Telchar daemon on the gateway host or VM. It is not the host's general-purpose `/nix/store`, and it is not shared with unrelated host workloads. The daemon is the only Telchar process permitted to query, mutate, retain, or garbage-collect this store.

```text
stock Nix client
  -- authenticated worker protocol --> Telchar frontend
  -- authenticated local IPC --> Telchar daemon
  -- Nix store API / daemon --> dedicated gateway store
```

The frontend, SSH account, executor workers, PostgreSQL client, and unrelated host services do not receive gateway-store access. Client authentication establishes membership in the mutually trusted store domain; it does not create per-path isolation.

## Service account and process ownership

- `telchar` service account runs the single active daemon and owns the gateway-store coordination process.
- The service account owns the gateway-store root, its Nix state, logs, temporary files, daemon socket, and runtime directories.
- The restricted SSH ingress account is unprivileged and may use only the authenticated local IPC endpoint.
- PostgreSQL stores control-plane state and leases; it is not a substitute for Nix store metadata or garbage collection.
- Executor accounts may access only the store endpoints explicitly required by their backend contract. They do not own or garbage-collect the gateway store.

The daemon must acquire the deployment's single-active fence before enabling store mutation or garbage collection. A second daemon must fail startup rather than share ownership.

## Daemon interaction

All store operations pass through the daemon's typed store boundary. The daemon validates the configured system and feature envelope, admission state, durable lease state, and Nix-store invariants before mutation. It uses the configured dedicated Nix daemon/store endpoint; no operation falls back to the client store or an unrelated host daemon.

The daemon owns:

- path validity and metadata queries;
- bounded NAR import and export;
- derivation and output retention leases;
- build admission and execution-result registration;
- garbage-collection coordination and observability.

The frontend must not open the Nix store socket, invoke `nix-store`, run a local build, or perform garbage collection. Client-provided store paths are data for typed validation, not authorization or endpoint selection.

## Required privileges

| Principal | Required access | Forbidden access |
| --- | --- | --- |
| `telchar` daemon | Own and use dedicated store/state/temp/log paths; connect to its Nix daemon; perform typed store operations and GC under service policy | Unrelated host workloads; client local stores; arbitrary host administration |
| SSH ingress account | Execute forced command; connect to private Telchar IPC socket | Gateway store, PostgreSQL, executor controls, arbitrary shell |
| PostgreSQL service/client | Database protocol and Telchar schema only | Nix store files, Nix daemon socket, client worker stream |
| Executor backend | Backend-specific staged inputs/outputs as explicitly leased | Gateway GC ownership, unrelated requests, ingress credentials |
| Host operator/root | Deployment administration and recovery | Not part of normal request execution or client trust |

No privilege is granted merely because a process can read a store path. Filesystem permissions, private socket directories, peer credentials, and service configuration enforce the boundary.

## Garbage collection ownership

Only the Telchar daemon may initiate or schedule gateway-store garbage collection. GC must honor durable leases for queued, running, collecting, and deliverable requests. Lease release and terminal request transitions are coordinated so GC cannot remove an input, derivation, output, or result still needed by an admitted request. Best-effort cache publication never extends correctness-critical retention.

Startup recovery reconciles stale leases and daemon state before enabling GC. Shutdown stops new admission, completes or fences store work, and removes only process-owned runtime artifacts; it does not delete the dedicated store as ordinary service cleanup.

## Excluded workloads

The dedicated gateway store must not receive host package management, unrelated system services, user profiles, arbitrary client local builds, or executor scratch data outside an explicit lease. Alternate store roots remain fixture/prototype concerns until real Nix behavior and deployment support are established.

## Verification checklist

- [x] Dedicated gateway-store owner and store root are named.
- [x] Service account and single-active daemon ownership are named.
- [x] Frontend and unrelated workloads are denied store access.
- [x] Required daemon privileges and forbidden privileges are listed.
- [x] Typed daemon interaction and no local-client fallback are explicit.
- [x] PostgreSQL, executor, and host-operator boundaries are distinct.
- [x] GC ownership and lease ordering are explicit.
- [x] Startup, shutdown, and recovery ownership are explicit.
- [x] Unrelated host workloads are excluded.
