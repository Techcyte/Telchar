# Nomad allocation transfer

## Status

Accepted for the MVP roadmap.

## Context

Telchar submits one deterministic Nomad batch job for an admitted shared build. Nomad owns placement and infrastructure autoscaling, but a placed allocation still needs the exact admitted Nix input closure, live build-log delivery, and a path for returning the exact declared outputs to Telchar's gateway store.

Nomad clients may be long-lived machines with warm Nix stores or newly autoscaled machines with cold stores. Operators may provide ordinary Nix substituters, a host Nix daemon, container-local store access, credentials, proxies, mounts, or other setup. Telchar must use those facilities without making any binary cache mandatory and without giving an allocation arbitrary access to the gateway store.

Nomad API traffic and allocation-to-Telchar traffic may run over HTTP on an operator-controlled trusted network or HTTPS where transport confidentiality and server authentication are required. Authentication remains mandatory even when the selected transport is HTTP.

## Decision

### Allocation-initiated transfer

The Nomad allocation initiates a connection to a configured Telchar transfer endpoint. Telchar does not discover Nomad client addresses or connect directly to arbitrary allocations.

The build path is:

```text
Telchar submits deterministic Nomad batch job
→ optional prestart task completes
→ allocation worker connects to Telchar
→ worker authenticates as the exact job and allocation
→ Telchar provides the admitted closure manifest
→ worker resolves locally available and substitutable paths
→ Telchar streams only unresolved admitted paths
→ worker builds through its configured Nix daemon
→ worker streams bounded live logs and exact declared outputs
→ Telchar validates and imports every output into the gateway store
→ shared build completes
```

A Nomad allocation reaching `complete` is not sufficient for build success. Telchar reports success only after every declared output has been transferred, validated, imported, and confirmed in the gateway store.

### Transfer transport

The Nomad API endpoint and the allocation transfer endpoint are separate operator-controlled URLs. Each explicitly supports either `http://` or `https://`.

Telchar does not redirect HTTP to HTTPS, infer a TLS requirement from Nomad configuration, or require TLS on a trusted network. HTTPS configuration fails closed on certificate or identity errors. HTTP provides no confidentiality; store paths, derivation metadata, logs, and NAR contents may be visible to the trusted network.

### Transfer authentication

Each Nomad backend selects one transfer-authentication mode.

`workload-identity` is the default. The operator configures the expected issuer, audience, and JWKS URL rather than requiring Telchar to infer a trust endpoint from the Nomad API URL. Telchar validates the JWT signature, issuer, audience, expiry, namespace, exact job ID, allocation ID, and task identity. HTTPS and custom CA settings for the JWKS endpoint are operator controlled. Workload identity over HTTP is permitted on a trusted network, but does not protect a bearer token from a network observer.

`hmac` is the built-in alternative. An operator-owned protected key file signs a short-lived, exact-build-scoped capability. Requests bind the key ID, protocol version, backend name, namespace, job ID, allocation ID when known, shared-build digest, expiry, nonce, method, path, and body digest. Telchar performs constant-time verification, bounded clock-skew checks, and replay rejection. The capability cannot enumerate or fetch paths outside the admitted closure and is never logged or stored in PostgreSQL. HMAC over HTTP provides authentication and integrity, not confidentiality.

Authentication mode and credentials are backend configuration. Client request bytes cannot select them.

### Optional prestart task

A Nomad backend may configure one optional lifecycle `prestart` task in the same deterministic job and task group as the build worker. It is not a separate job or execution identity.

The task is generic operator-controlled Nomad task configuration with its own driver, bounded resources, finite timeout, and bounded `driver_config`. It may configure cache credentials, proxies, mounts, Nix settings, shared allocation directories, or other build-environment prerequisites. Client fields are never interpolated into its command, arguments, image, environment, or driver configuration. Failure or timeout prevents the build task from starting and terminates the same shared-build attempt without automatic retry.

### Allocation-side Nix store

The worker uses an operator-selected Nix daemon endpoint. A host Nix daemon socket is a first-class mode and is expected to be the efficient common deployment:

```text
allocation worker
→ mounted or directly available host nix-daemon socket
→ persistent host Nix store
```

A warm host store acts as a best-effort local cache across allocations. A cold or autoscaled host may populate missing paths from its operator-configured substituters. Exposing a host Nix daemon socket to a task grants substantial authority over that host's Nix store; mounts, daemon trust, sandboxing, and task privileges remain explicit operator responsibilities.

Telchar does not inject arbitrary host mounts or allow clients to select a Nix daemon, store URI, substituter, public key, credential, or cache policy.

### Input closure

Telchar computes the complete transitive closure of the admitted derivation and input roots from the gateway store. The complete bounded manifest is sent to the worker and contains exact store-path identity and verification metadata. Sending the manifest does not imply sending every NAR body.

The worker resolves inputs in this order:

1. Query the configured allocation-side Nix daemon for already-valid manifest paths.
2. Ask that daemon to use its operator-configured substituters for absent paths.
3. Recheck exact manifest paths and their store validity.
4. Request only still-missing paths from Telchar.

Telchar authorizes requests only for paths in the admitted manifest. Each unresolved path is streamed independently from the gateway store using bounded buffers and normal Nix NAR operations, then imported through the allocation-side Nix daemon. Telchar does not buffer a complete NAR or closure in memory.

Consequently, common public dependencies such as GCC normally come from a warm host store or an operator-trusted binary cache. Telchar transfers GCC only as a correctness fallback when it is part of the admitted closure, exists in the gateway store, and remains unresolved after local and substitution checks. Private or unpublished inputs normally use the Telchar fallback.

A binary cache improves transfer efficiency but is not required for correctness.

### Build and logs

After verifying the complete input closure, the worker invokes trusted normal-mode `BuildDerivation` through its configured Nix daemon. The worker emits bounded protocol records and live log chunks to Telchar.

The coordinator broadcasts live chunks to currently attached local clients through bounded queues. Slow or disconnected clients cannot block the allocation or cause unbounded retention. PostgreSQL stores no log bytes and historical replay is not promised.

### Output transfer

After remote build success, the worker queries only the exact declared output paths from the allocation-side Nix daemon. It sends bounded metadata and streams each output NAR independently to Telchar.

Telchar reuses the gateway import and validation pipeline to verify the declared path, NAR hash and size, references, deriver, content-address metadata where applicable, NAR structure, and exact expected-output set before authoritative registration. Every declared output must validate and be confirmed by the gateway store. Missing, extra, malformed, corrupt, unverifiable, or mismatched output data fails closed as an output failure.

Executor trust does not bypass gateway transport and store validation. Classic input-addressed output honesty remains within the trusted-executor boundary documented by the project design.

### Durability and restart

PostgreSQL stores bounded execution and transfer metadata, including the exact backend, Nomad job ID, allocation ID when known, transfer phase, manifest digests, and terminal outcome. It never stores credentials, NAR bodies, or log bytes.

After restart, Telchar checks exact gateway outputs first. Otherwise it reconnects only to the original configured Nomad backend and exact persisted job identity, verifies the allocation identity, and resumes monitoring or an idempotent transfer phase. Re-sending a hash-verified store object may be transport recovery. Launching another allocation or repeating `BuildDerivation` is an execution retry and is not automatic.

### Bounds

The transfer protocol has a fixed version and explicit operator-owned limits for manifest path count and bytes, individual and aggregate input bytes, individual and aggregate output bytes, frame metadata, streaming buffers, live-log queues, transfer idle time, setup time, build runtime, output collection, authentication lifetime, clock skew, nonce retention, reconnect time, and diagnostic capture.

Existing retained-input accounting continues to protect gateway-store retention. Transfer limits use separately named settings where their meaning differs; one limit is not silently reused for unrelated resource policies.

## Consequences

Telchar can use persistent Nomad-client Nix stores and ordinary binary caches for efficiency while retaining a private-input fallback and exact gateway-store authority. Autoscaled cold nodes remain correct. Operators choose HTTP or HTTPS and choose workload identity or HMAC without exposing those choices to Nix clients.

The design requires a small packaged Nomad allocation worker and a bounded authenticated transfer protocol. The worker does not schedule jobs, access PostgreSQL, choose stores or caches, provision infrastructure, or retry failed builds.

The optional prestart task provides an open operator extension point without adding another Nomad job or durable execution identity. Its flexibility also carries operator responsibility: arbitrary task configuration and host-daemon access can be privileged and must be reviewed as deployment policy.
