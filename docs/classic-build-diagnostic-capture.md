# Classic-build diagnostic capture

**Status: diagnostic discovery only — not compatibility acceptance evidence.**

## Method

Each run creates a new `NixFixture`, starts its fixture-owned daemon with
`nix-daemon --debug`, verifies trust with `nix --store unix://<fixture-socket>
store info --json`, then runs the fixed classic input-addressed build defined
in [classic-build-fixtures.md](classic-build-fixtures.md). The stock client is
run with `--debug`; `NixDaemon::diagnostic_operations` extracts only decimal
worker operation codes from the client's bounded debug lines. Fixture cleanup
removes the daemon and all files after each run.

The capture retains neither a NAR body, derivation body, output body, credential, socket path, nor unbounded string. It does not inspect worker transport bytes, infer frame boundaries from transport chunks, or treat chunk metadata as protocol shape. Candidate operations come only from explicit Nix
client instrumentation; candidate response, callback, upload, activity, error,
and result classifications come only from the pinned execution path and primary
source serializers below.

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

## Candidate flow classes from the pinned execution path

The primary source proves each operation's daemon dispatch and serializer path;
the explicit client instrumentation proves the operation sequence above. This
is discovery evidence only, not a parser or relay design. `AddToStore` is a candidate upload boundary.

| Candidate | Evidence | Direction and classification | Uncertainty |
| --- | --- | --- | --- |
| `SetOptions` | `RemoteStore::setOptions`; `daemon.cc::performOp` | client request; daemon terminal frame | Debug instrumentation identifies operation only. |
| `AddTempRoot` | `BasicClientConnection::addTempRoot`; `daemon.cc::performOp` | client request; daemon terminal frame and result word | Debug instrumentation identifies operation only. |
| `IsValidPath` | `RemoteStore::isValidPathUncached`; `daemon.cc::performOp` | client request; daemon terminal frame and result word | Debug instrumentation identifies operation only. |
| `AddToStore` | `RemoteStore::addCAToStore`; `daemon.cc::performOp` | client request, framed upload, daemon terminal frame, `ValidPathInfo` response | Upload is a candidate boundary; body is neither read nor retained. |
| `QueryMissing` | `RemoteStore::queryMissing`; `daemon.cc::performOp` | client request; daemon terminal frame and missing-path result | Exact serializer remains T036F work. |
| `QueryPathInfo` | `BasicClientConnection::queryPathInfo`; `daemon.cc::performOp` | client request; daemon terminal frame and optional path-info result | Exact serializer remains T036F work. |
| `BuildPathsWithResults` | `RemoteStore::buildPathsWithResults`; `daemon.cc::performOp` | client request; daemon terminal frame and keyed build-result vector | Fixed fixture did not emit an observed callback, activity, error, or result frame. |
| `STDERR_READ` / `STDERR_WRITE` | `daemon.cc::TunnelSource` / `TunnelSink`; `worker-protocol-connection.cc::processStderrReturn` | possible bidirectional callback boundary | Not observed in the fixed fixture; candidate only because the execution path can invoke it. |
| `STDERR_START_ACTIVITY`, `STDERR_STOP_ACTIVITY`, `STDERR_RESULT` | `daemon.cc::TunnelLogger`; `worker-protocol-connection.cc::processStderrReturn` | possible daemon-to-client activity/result frames | Not observed in the fixed fixture; candidate only because the execution path can emit them. |
| `STDERR_ERROR` | `TunnelLogger::stopWork`; `processStderrReturn` | possible daemon-to-client error frame | Not observed because both fixed fixture builds succeed. |
| `STDERR_LAST` | `TunnelLogger::stopWork`; `processStderrReturn` | daemon-to-client terminal frame after each request | Established by execution-path control flow, not transport capture. |

No callback or response classification is accepted from debug text. Candidate
classes marked possible are not evidence that the fixed fixture emitted them.

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
