# Nix compatibility matrix

## Compatibility target

| Field | Value |
| --- | --- |
| Client | Stock Nix 2.34.8 |
| Client provenance | Flake-locked `NixOS/nixpkgs` revision `04607e1165ac22c5fde6dcc54c9e0b3c0487c555` |
| Worker protocol | Accept 1.18 through 1.38; capture the negotiated version for each trace |
| Lix | Deferred; not supported until its own real-client compatibility traces pass |

Nix 2.34.8 is the package exposed as `nix-reference` by this repository's locked flake on `x86_64-linux`. The gate derives this version from `.#nix-reference.version` and invokes the built executable by absolute store path rather than depending on the development shell's `nix` command.
The pinned package source resolves to the revision recorded by the flake lock and protocol evidence inventory.
Its `src/libstore/worker-protocol.cc` defines worker-protocol latest as 1.38 and
minimum as 1.18. The supported range here describes the pinned client's
negotiation range; Telchar support remains limited to matrix rows with recorded
real-client evidence.

## Initial matrix

| Client | Protocol range | Trust mode | Derivation class | Required trace evidence | Support state |
| --- | --- | --- | --- | --- | --- |
| Stock Nix 2.34.8 | 1.18–1.38 | Trusted | Classic input-addressed | `trusted-classic-build-v1`: typed handshake `1.38`/`1.38`, `trusted:true`, then `SetOptions`, `AddTempRoot`, `IsValidPath`, `AddToStore`, `QueryMissing`, `QueryPathInfo`, `BuildPathsWithResults`; one fixture-store output hashes to `984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f` | Trusted trace accepted |
| Stock Nix 2.34.8 | 1.18–1.38 | Untrusted | Classic input-addressed | `untrusted-classic-build-v1`: typed handshake `1.38`/`1.38`, `trusted:false`, then `SetOptions`, `AddTempRoot`, `IsValidPath`, `AddToStore`, `QueryMissing`, `QueryPathInfo`, `BuildPathsWithResults`; one fixture-store output hashes to `984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f` | Untrusted trace accepted |
| Stock Nix 2.34.8 | 1.18–1.38 | Trusted or untrusted | Content-addressed | Unsupported for MVP: classic input-addressed traces do not cover content-addressed behavior | Deferred pending concrete evidence |
| Lix | Not recorded | Not recorded | Not recorded | Separate real-client trace packet | Deferred |

The trusted trace runs the exact fixed fixture contract in
`docs/classic-build-fixtures.md` through the test-only `TraceCapture` proxy to a
real Nix daemon; its boundary coverage is the classic-build inventory in
`docs/protocol-fixture-flow-inventory.md`. The trace stores only negotiated
versions, the typed trust outcome, and operation classifications. It stores
neither the output path nor any request, response, upload, derivation, NAR,
secret, or raw protocol body.

These rows prove observed client↔Nix-daemon fixture traffic and typed transparent
observation. They do **not** mean the Telchar daemon can execute those builds.
The production daemon currently completes handshake, post-handshake information,
and `SetOptions`; it returns a Nix-compatible framed rejection for every later
store/build operation. End-to-end Telchar build support remains pending Gate 3,
including T076–T089 and the required-operation coverage task T086A.

## Content-addressed deferral

Content-addressed builds are **Unsupported for MVP**. The accepted classic
input-addressed traces do not imply content-addressed compatibility, even where
they use the classic fixture's typed `AddToStore` staging operation. The
observer fails closed for any content-addressed build-specific flow.

Admission requires all of: a concrete content-addressed fixture; an inventory
of required operations and result semantics; primary pinned-Nix serializer or
documentation evidence for every boundary and protocol-version condition;
finite accepted metadata limits; typed observer coverage for every request,
response, callback, and upload path; negative tests for malformed, oversized,
and unknown flows; and a real-client trace with an approved output proof in
both applicable trust modes. These prerequisites require a separately planned
compatibility packet.

No row is supported before its trace evidence is recorded. Required operations,
optional operations, recognized-rejected operations, and unknown-operation
behavior are defined after T013–T015 in the T016 allowlist.
