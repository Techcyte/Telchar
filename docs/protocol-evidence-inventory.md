# Independent protocol evidence inventory

## Evidence policy

This inventory covers only behavior required by the accepted fixed classic
input-addressed fixtures at flake-pinned Nix 2.34.8. Captured acceptance evidence is
limited to the bounded sanitized artifacts
`docs/compatibility-traces/trusted-classic-build-v1.json` and
`docs/compatibility-traces/untrusted-classic-build-v1.json`; they contain
fixture ID, negotiated versions, typed trust outcome, operation codes, and an
approved output hash only.

Primary evidence is the cited pinned-Nix source and serializers, plus the
captured typed traffic. Rio contributes only architecture or test-category notes,
never wire behavior, serializers, operation semantics, or implementation
requirements. No required behavior relies on Rio implementation details.

## Required behavior inventory

| Required behavior | Captured typed traffic | Primary Nix evidence | Independent implementation and test task |
| --- | --- | --- | --- |
| Primitive framing and finite body handling | Every trace boundary uses worker words and padded strings; `AddToStore` has one bounded framed upload | `src/libutil/include/nix/util/serialise.hh`, `src/libutil/serialise.cc` | T023–T030: read/write primitives, truncation and allocation limits, property tests, fuzz target |
| Worker handshake and version/trust negotiation | Both traces negotiate client/peer `1.38`; trusted trace records `true`, untrusted trace records `false` | `src/libstore/worker-protocol-connection.cc`, `src/libstore/worker-protocol.cc`, `src/nix/unix/daemon.cc::authPeer` | T031–T035: magic parsing/emission, version negotiation/rejection, real-client handshake |
| Operation classification | Both traces classify `19, 11, 1, 7, 40, 26, 46`; classifier reports zero unclassified codes | `src/libstore/include/nix/store/worker-protocol.hh`, `src/libstore/daemon.cc::performOp` | T036: operation-code parser; T016: fixture allowlist and classifier |
| `SetOptions` (`19`) | Required client request before classic operations; terminal reply | `src/libstore/remote-store.cc::setOptions`, `src/libstore/daemon.cc::performOp` | T036A–T036B: inventory and typed parser; T036C and T036H: transparent relay |
| `AddTempRoot` (`11`) | Optional typed request present in both accepted traces | `src/libstore/worker-protocol-connection.cc::BasicClientConnection::addTempRoot`, `src/libstore/daemon.cc::performOp` | T036A–T036B; T036H |
| `IsValidPath` (`1`) | Required typed store-path query | `src/libstore/remote-store.cc::isValidPathUncached`, `src/libstore/daemon.cc::performOp` | T036A–T036B; T036H |
| `AddToStore` (`7`) | Required bounded framed classic staging upload and typed path-info result | `src/libstore/remote-store.cc::addCAToStore`, `src/libstore/daemon.cc::performOp`, `src/libutil/include/nix/util/serialise.hh` | T036A–T036B; T036H streaming relay and boundary test |
| `QueryMissing` (`40`) | Required typed derived-path request and result collections | `src/libstore/remote-store.cc::queryMissing`, `src/libstore/daemon.cc::performOp`, `src/libstore/include/nix/store/length-prefixed-protocol-helper.hh` | T036A–T036B; T036H |
| `QueryPathInfo` (`26`) | Required typed path-info query and conditional response | `src/libstore/worker-protocol-connection.cc::BasicClientConnection::queryPathInfo`, `src/libstore/daemon.cc::performOp` | T036A–T036B; T036H |
| `BuildPathsWithResults` (`46`) | Required fixed classic build request and typed result vector | `src/libstore/remote-store.cc::buildPathsWithResults`, `src/libstore/daemon.cc::performOp`, `src/libstore/common-protocol.cc` | T036A–T036B; T036H |
| Activity and terminal frames | Typed `STDERR_NEXT`, activity start/stop/result, and terminal frames on operation replies | `src/libstore/daemon.cc::TunnelLogger`, `src/libstore/worker-protocol-connection.cc::processStderrReturn`, `src/libstore/include/nix/store/worker-protocol.hh` | T036A–T036B; T036H |
| Unsupported callbacks, uploads, and content-addressed paths | No accepted trace contains callback, untyped upload, or content-addressed build-specific flow; observer rejects before reading body | `src/libstore/include/nix/store/worker-protocol.hh`, `src/libstore/daemon.cc::performOp` | T012–T016: fail-closed trace capture, content-addressed deferral, allowlist classification |

The exact version gates, typed shapes, finite fixture limits, and fail-closed
outcomes are maintained in `docs/protocol-fixture-flow-inventory.md`. The
compatibility matrix and allowlist map those behaviors to the trusted and
untrusted acceptance traces. This inventory does not make an unsupported
content-addressed behavior supported.
