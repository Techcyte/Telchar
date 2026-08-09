# Pure-Rust Nix-daemon client boundary

## Status

Accepted for T095A implementation planning.

## Decision

Telchar accesses the gateway store through a typed pure-Rust worker-protocol client:

```text
Telchar Rust
→ caller-owned typed nix-worker-protocol client
→ deployment-configured gateway nix-daemon Unix socket
```

The worker protocol is the compatibility boundary. Telchar does not bind the Nix C++ ABI, use FFI, invoke Nix commands, discover executables through `PATH`, fall back to the host/default store, or accept a client-selected store endpoint.

The existing `WorkerClient<S>` root-registration subset in `crates/nix-worker-protocol` is extended rather than replaced. The reusable crate owns worker framing, negotiation, typed operations, bounded parsing, and protocol errors. Telchar owns endpoint configuration, Unix connection lifecycle, deadlines, cancellation, domain validation, persistence, roots, quotas, and telemetry.

## Fixed compatibility profile

The initial profile targets the flake-pinned Nix 2.34.8 worker protocol:

```text
client maximum: 1.38
client minimum: 1.18
required major: 1
logical store: /nix/store
```

The client sends worker magic 1 and version 1.38. It requires worker magic 2, major version 1, and daemon version at least 1.18. The negotiated number is `min(daemon, 1.38)`. A daemon newer than 1.38 is accepted only through the 1.38 intersection. A different major or version below 1.18 fails closed.

For negotiated 1.38, both sides exchange bounded feature sets. Telchar advertises no optional features until a later accepted operation requires one. Any received feature set is bounded by count, element size, aggregate retained metadata, and zero-padding validation. Unknown optional features are ignored after bounded decode and are not retained. Required semantics are represented by typed capabilities, never by scattered raw version checks in Telchar.

## Post-handshake profile

The client emits the obsolete compatibility fields required by the negotiated protocol:

```text
>= 1.14: CPU affinity = 0
>= 1.11: reserve space = false
```

It then reads bounded post-handshake information:

```text
>= 1.33: daemon Nix version string
>= 1.35: optional trust value
```

The daemon version string is validated and discarded. It must not be exposed in telemetry or retained in the profile.

Trust is normalized as:

```rust
pub enum WorkerTrust {
    Trusted,
    Untrusted,
    Unknown,
}
```

`Unknown` applies below protocol 1.35 or when the optional trust field is absent. Values outside the pinned optional-boolean encoding fail closed. Trust describes whether the daemon reports that it trusts this client connection. It does not prove output provenance, signatures, reproducibility, or content safety.

The connection consumes the startup STDERR/activity stream through exactly one terminal frame. A daemon error becomes one bounded redacted protocol error. Daemon text, paths, IDs, URLs, SQL, credentials, NARs, derivations, environment contents, logs, and activity fields are never returned to callers or telemetry.

## Capability model

Capabilities describe implemented semantics, not operation-code availability. The reusable profile exposes only capabilities backed by an accepted typed serializer/parser and a sufficient negotiated protocol.

Initial capability thresholds:

| Capability | Minimum | Purpose |
| --- | ---: | --- |
| root registration | 1.18 | `AddTempRoot`, `AddIndirectRoot` with accepted exact-root lifecycle |
| path validity | 1.18 | `IsValidPath` |
| path information | 1.18 | `QueryPathInfo`; conditional-valid response semantics are supported at 1.18+ |
| raw NAR export | 1.18 | `NarFromPath` streaming |
| complete input closure | 1.18 | bounded recursive composition of `QueryPathInfo.references`; exact semantics remain `computeFSClosure(roots, false, false, false)` for the accepted no-flip/no-outputs/no-derivers case |
| metadata-preserving NAR registration | 1.18 | `AddToStoreNar`, not content-addressed `AddToStore` |
| basic derivation execution | 1.18 | `BuildDerivation` with classic input-addressed `BasicDerivation` and normalized `BuildResult` |

Negotiation alone does not activate unfinished capabilities. `WorkerClientCapabilities` reports a capability only when both the version requirement and the compiled typed implementation are present. Telchar must not infer support from a raw operation number.

Content-addressed build behavior, dynamic derivations, repair mode, substitutions, signature bypass, recursive daemon behavior, and BuildPaths-with-results are outside this migration unless separately accepted.

## Operation migration map

The native helpers are replaced in this order:

1. input closure;
2. metadata-preserving import/promotion;
3. raw NAR export;
4. local `BasicDerivation` build;
5. helper packaging and environment removal;
6. full Gate 3 with helpers unavailable.

Required typed operations:

```text
IsValidPath
QueryPathInfo
NarFromPath
AddToStoreNar
BuildDerivation
AddTempRoot
AddIndirectRoot
bounded `QueryPathInfo.references` traversal fixed by T095C differential evidence
```

Each operation owns:

- exact pinned serializer order and version gates;
- bounded counts, strings, paths, references, signatures, hashes, and result metadata;
- streaming bodies with bounded memory and backpressure;
- STDERR/activity handling through one typed terminal state;
- fail-closed malformed, truncated, oversized, unsupported, and trailing input behavior;
- differential real-private-daemon evidence against the accepted Gate 3 behavior.

## Connection ownership

`nix-worker-protocol` remains generic over caller-owned `Read + Write` streams and performs no socket or environment work.

Telchar's gateway connection layer:

- reads only `TELCHAR_GATEWAY_STORE_URI` from deployment configuration;
- accepts only the approved absolute `unix:///...` form;
- connects directly to that Unix socket;
- never uses client bytes, `PATH`, `NIX_STORE`, a default socket, or host-store fallback;
- creates a fresh owned connection for an operation unless an explicitly bounded reusable owner is later accepted;
- marks a connection unusable after any timeout, cancellation, protocol error, I/O error, incomplete operation, or unexpected terminal state.

The logical store directory is fixed to `/nix/store`. Endpoint and logical store identity are deployment concerns and do not appear in reusable protocol types.

## Timeout, cancellation, and owner death

Telchar owns deadlines because generic streams do not provide portable timeout controls. Before handing a stream to the reusable client, Telchar configures bounded read/write behavior appropriate to the Unix connection.

On timeout, cancellation, requester policy action, daemon failure, owner death, parser failure, or writer/reader failure:

```text
shutdown both directions
→ discard connection
→ join/reap owning operation thread if present
→ return bounded domain error
```

A timed-out or partially consumed connection is never reused. No background operation may outlive its Telchar owner. Existing synchronous thread-per-session architecture remains unchanged through MVP.

## Error mapping

Reusable errors are typed and bounded by failure class:

```text
configuration is not a protocol-crate error
I/O
protocol mismatch
unsupported profile/capability
malformed or oversized response
remote operation rejected
cancelled
timed out
```

Remote diagnostics are consumed for framing correctness but redacted from public errors. Telchar maps these classes into existing domain errors without exposing endpoints, store paths, request/session/lease IDs, deadlines, SQL, credentials, NARs, derivations, logs, or daemon text.

## Trust and authorization

The configured local gateway daemon is a deployment-owned privileged boundary. Telchar requires a profile compatible with each requested operation. For operations whose Nix daemon authorization depends on trust, an explicit `Untrusted` result fails before sending the operation. `Unknown` fails closed for classic input-addressed `BuildDerivation` and metadata registration with signature checking disabled. Read-only path queries/export and root registration may accept `Unknown` when their operation packet proves the pinned daemon permits the behavior. Because the pinned migration profile is 1.18–1.38, protocols below 1.35 cannot provide a positive trust assertion and therefore do not expose trust-required capabilities.

Output trust remains `TrustedExecutor` in Telchar's normalized classic result because it describes the configured executor boundary. Worker trust does not elevate that to provenance proof.

## Resource bounds

All retained protocol metadata charges the existing session/allocation budget or a narrower operation budget. The client must bound before allocation:

- feature count and aggregate feature bytes;
- daemon version string;
- activity field count and bytes;
- remote error/trace fields;
- store paths and path sets;
- references and signatures;
- derivation outputs, inputs, arguments, and environment entries;
- result collections;
- framed upload/download chunks.

Complete NARs, derivations, logs, environment bodies, feature strings, daemon version strings, and remote diagnostics are not retained. NAR and log bodies stream with bounded backpressure.

## Rejected alternatives

### Nix C++ ABI or Rust FFI

Rejected. The ABI is not the compatibility contract, complicates pinning and packaging, and introduces unsafe ownership and exception boundaries.

### Shelling out to `nix` or `nix-store`

Rejected. It adds subprocess lifecycle, text/JSON compatibility, stderr capture, PATH/version discovery, and cancellation complexity already demonstrated by the compatibility helpers.

### Keep the native compatibility helpers

Rejected as the long-term boundary. They remain only until differential migration tasks preserve the accepted Gate 3 baseline.

### Direct local-store filesystem or database access

Rejected. It bypasses daemon locking, authorization, metadata validation, and protocol compatibility.

### Async rewrite during migration

Rejected. Gate 3 has a verified synchronous thread-per-session architecture. Async remains a measured post-MVP decision.

## Verification contract

T095B and later packets require both hostile scripted streams and a parent-owned private Nix daemon fixture. Final acceptance requires:

```text
native helper binaries absent
native helper environment variables absent
no helper subprocesses observed
full Gate 3 protocol/store/OpenSSH/GC/lifecycle suite green
```

The source authority is the flake-pinned Nix 2.34.8 implementation, especially:

```text
src/libstore/worker-protocol.cc
src/libstore/worker-protocol-connection.cc
src/libstore/include/nix/store/worker-protocol.hh
src/libstore/include/nix/store/worker-protocol-connection.hh
src/libstore/remote-store.cc
src/libstore/daemon.cc
```

## Consequences

Telchar gains one typed compatibility boundary shared by root registration, store queries, transfer, import, and execution. Native helper removal becomes incremental and differential rather than a rewrite. Protocol drift fails closed. Endpoint ownership, retention, lifecycle, telemetry, and scheduling remain Telchar domain concerns rather than leaking into the reusable wire crate.
