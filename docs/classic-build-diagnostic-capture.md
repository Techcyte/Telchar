# Classic-build diagnostic capture

**Status: diagnostic discovery only — not compatibility acceptance evidence.**

## Method

Each run creates a new `NixFixture`, starts its fixture-owned daemon with
`nix-daemon --debug`, verifies trust with `nix --store unix://<fixture-socket>
store info --json`, then runs the fixed classic input-addressed build defined
in [classic-build-fixtures.md](classic-build-fixtures.md). The stock client is
run with `--debug`; `NixDaemon::diagnostic_operations` extracts only decimal
worker operation codes from its bounded stderr lines. It retains neither a
NAR body, derivation body, output body, credential, socket path, nor unbounded
string. Retained-data prohibition: it retains neither a NAR body, derivation body, output body, credential, socket path, nor unbounded string. Fixture cleanup removes the daemon and all files after each run.

The capture runs each fixture twice. Candidate equality demonstrates only
repeatable diagnostic classification, never protocol support.

## Result

Pinned Nix 2.34.7, source revision
`2c6d06e9387cf58167cb5a7ab91cee7333d8d17c`, produced the following candidate
client-to-server operation sequence in both repetitions of both fixtures:

| Trust mode | Pre-build trust result | Candidate operation codes | Candidate names |
| --- | --- | --- | --- |
| Trusted | `true` | `19, 11, 1, 7, 40, 26, 46` | `SetOptions`, `AddTempRoot`, `IsValidPath`, `AddToStore`, `QueryMissing`, `QueryPathInfo`, `BuildPathsWithResults` |
| Untrusted | `false` | `19, 11, 1, 7, 40, 26, 46` | `SetOptions`, `AddTempRoot`, `IsValidPath`, `AddToStore`, `QueryMissing`, `QueryPathInfo`, `BuildPathsWithResults` |

`AddToStore` is a candidate upload boundary. The diagnostic capture does not
inspect, retain, classify, or accept its body. `BuildPathsWithResults` can
produce activity, result, error, callback, and terminal worker frames; this
capture records no raw response frame. No callback or response classification
is accepted from debug text. No callback or response classification is accepted from debug text.

## Primary-source investigation targets

Before inventory or parser acceptance, inspect these pinned-Nix serializers
and dispatch sites at the stated revision:

- `src/libstore/remote-store.cc`: `RemoteStore::addToStore`,
  `RemoteStore::queryMissing`, `RemoteStore::queryPathInfo`, and
  `RemoteStore::buildPathsWithResults` request serializers;
- `src/libstore/daemon.cc::performOp`: corresponding operation decoding,
  response serializers, and `TunnelSource` / `TunnelSink` callback behavior;
- `src/libstore/include/nix/store/worker-protocol.hh`: operation and stderr
  frame constants;
- `src/libstore/worker-protocol-connection.cc` and
  `src/libstore/worker-protocol.cc`: versioned handshake and post-handshake
  serialization;
- `src/libutil/include/nix/util/serialise.hh` and
  `src/libutil/serialise.cc`: integer, string, and framed stream primitives.

No candidate flow is supported until the versioned inventory provides exact
serializer boundaries and finite limits, parser golden/truncation tests pass,
and transparent relay coverage proves byte preservation and fail-closed
behavior.
