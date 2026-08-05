# Nix compatibility matrix

## Compatibility target

| Field | Value |
| --- | --- |
| Client | Stock Nix 2.34.7 |
| Client provenance | Flake-locked `NixOS/nixpkgs` revision `04607e1165ac22c5fde6dcc54c9e0b3c0487c555` |
| Worker protocol | Accept 1.18 through 1.38; capture the negotiated version for each trace |
| Lix | Deferred; not supported until its own real-client compatibility traces pass |

Nix 2.34.7 is the package exposed by this repository's flake on `x86_64-linux`.
The Nix 2.34.7 tag resolves to commit `2c6d06e9387cf58167cb5a7ab91cee7333d8d17c`.
Its `src/libstore/worker-protocol.cc` defines worker-protocol latest as 1.38 and
minimum as 1.18. The supported range here describes the pinned client's
negotiation range; Telchar support remains limited to matrix rows with recorded
real-client evidence.

## Initial matrix

| Client | Protocol range | Trust mode | Derivation class | Required trace evidence | Support state |
| --- | --- | --- | --- | --- | --- |
| Stock Nix 2.34.7 | 1.18–1.38 | Trusted | Classic input-addressed | Handshake and operation sequence | Pending T013 trace |
| Stock Nix 2.34.7 | 1.18–1.38 | Untrusted | Classic input-addressed | Handshake, trust negotiation, and operation sequence | Pending T014 trace |
| Stock Nix 2.34.7 | 1.18–1.38 | Trusted or untrusted | Content-addressed | Operation and result semantics, or explicit deferral | Pending T015 resolution |
| Lix | Not recorded | Not recorded | Not recorded | Separate real-client trace packet | Deferred |

No row is supported before its trace evidence is recorded. Required operations,
optional operations, recognized-rejected operations, and unknown-operation
behavior are defined after T013–T015 in the T016 allowlist.
