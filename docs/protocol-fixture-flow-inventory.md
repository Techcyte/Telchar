# Typed fixture-flow inventory

## Scope and evidence

This inventory covers every protocol flow reachable from the current compatibility fixtures at the flake-pinned Nix 2.34.8 source tag.

| Fixture | Client command | Reachable worker flow |
| --- | --- | --- |
| `crates/telchar/tests/stdio_handshake.rs` | `nix --store ssh-ng://telchar-handshake-test store info` | Worker handshake, post-handshake information, `SetOptions`, terminal activity frame |
| `crates/telchar/tests/worker_trace.rs` | `nix --store unix://… store info --json` | Worker handshake, post-handshake information, `SetOptions`, terminal activity frame |

Handshake acceptance is not a compatibility or support claim for untested Nix releases. The concrete current fixtures terminate after `SetOptions`; primary dispatch/serializer evidence in `remote-store.cc` and `daemon.cc` shows neither fixture sends `AddToStore`, `AddToStoreNar`, or `AddMultipleToStore`, and their `SetOptions` replies are `STDERR_LAST`. Therefore no callback or upload is reachable. `STDERR_READ` and `STDERR_WRITE` are callback tags defined in `worker-protocol.hh`, but are never emitted by this fixture path.

## Versioned message inventory

All integer fields are 64-bit little-endian words. Byte strings are an integer byte length, body, and zero padding through the next 8-byte boundary. This framing is defined by `src/libutil/include/nix/util/serialise.hh` and `src/libutil/serialise.cc`.

| Direction | Boundary and exact shape | Version condition | Bound | Primary Nix source |
| --- | --- | --- | --- | --- |
| Client → server | `WORKER_MAGIC_1`, client version, feature-string set | feature set at `>= 1.38` | 64 features; 1024 bytes each | `src/libstore/worker-protocol-connection.cc`, `src/libstore/include/nix/store/worker-protocol.hh` |
| Server → client | `WORKER_MAGIC_2`, server version, feature-string set | feature set at negotiated `>= 1.38` | 64 features; 1024 bytes each | `src/libstore/worker-protocol-connection.cc` |
| Client → server | obsolete CPU-affinity word, obsolete reserve-space word | CPU affinity at `>= 1.14`; reserve space at `>= 1.11` | fixed words | `src/libstore/worker-protocol-connection.cc` |
| Server → client | daemon Nix version string, optional trust-status word | version string at `>= 1.33`; trust status at `>= 1.35` | 1024-byte daemon version; trust tag is `0`, `1`, or `2` | `src/libstore/worker-protocol.cc` |
| Server → client | `STDERR_LAST` | always after post-handshake information | fixed word | `src/libstore/include/nix/store/worker-protocol.hh`, `src/libstore/remote-store.cc` |
| Client → server | `SetOptions` operation `19`, twelve fixed setting words, override count, name/value string pairs | all pinned fixture versions | 256 override pairs; 16384 bytes per name or value | `src/libstore/remote-store.cc`, `src/libstore/daemon.cc` |
| Server → client | `STDERR_LAST` response to `SetOptions` | all pinned fixture versions | fixed word | `src/libstore/daemon.cc` |

For the two narrow `store info` fixtures, `SetOptions` is the only fixture-reachable operation. Its exact request boundary begins at its typed operation word; the next message cannot begin until all twelve fixed words, override count, and each declared name/value pair have been consumed. The observer retains only operation code, negotiated version, frame kind, declared string lengths, override count, and terminal frame kind. It retains no feature, daemon-version, override-name, or override-value body. The classic-build fixtures below exercise the larger typed operation set documented in their own inventory.

## Stock-Nix Gate 3 walking skeleton

The parent-owned production walking skeleton uses stock Nix 2.34.8 through
`ssh-ng://` and currently reaches `BuildDerivation` (`36`) after an empty
`AddMultipleToStore` (`44`). Operation 36 carries a derivation store path, a
full `BasicDerivation` encoded by `writeDerivation`, and a `BuildMode`; its
response is one `BuildResult`. This boundary is authoritative for production
packet order even though the older diagnostic classic-build fixtures below
expose `BuildPathsWithResults` (`46`).

## Classic-build fixture inventory

`crates/telchar/tests/classic_build_envelope.rs` is a test-only, disposable
observer for the trusted and untrusted fixture-owned daemons in
`crates/telchar/tests/nix_fixture.rs`. It is discovery support, not production
relay code and not compatibility acceptance evidence. It reads a typed boundary
only after the serializer below identifies it, forwards bodies through a fixed
4096-byte buffer, verifies worker-string zero padding, and records only the
maximum declared length or count. It never retains a NAR, derivation, path,
option, activity message, signature, error text, or other body.

All rows below apply only to the pinned 2.34.8 fixture, negotiated at `1.38`.
The finite values are **P003C fixture acceptance limits**, produced by two
successful runs in each trust mode. They are observations of this exact
fixture—not generic Nix protocol limits, service limits, or support for
arbitrary builds. A future fixture exceeding one requires an intentional
inventory, golden-test, and fixture-acceptance-limit update before admission.

| Direction | Boundary and exact shape | Pinned version gate and primary serializer | P003C fixture acceptance limit | Retained metadata / fail-closed behavior |
| --- | --- | --- | --- | --- |
| client → daemon | `SetOptions` (`19`), twelve words, override count, name/value padded strings | `RemoteStore::setOptions`; `daemon.cc::performOp(SetOptions)` | 2 pairs; name ≤17, value ≤85 | operation and declared maxima only; another operation/order rejects |
| client → daemon | `AddTempRoot` (`11`) and `IsValidPath` (`1`), each one `StorePath` string | `BasicClientConnection::addTempRoot`, `RemoteStore::isValidPathUncached`; `CommonProto::Serialise<StorePath>`; `daemon.cc::performOp` | request path ≤153 | declared maximum only; malformed padding rejects |
| daemon → client | terminal/activity stream before every reply: `STDERR_NEXT` string, `STDERR_START_ACTIVITY` (three words, string, counted typed fields, parent), `STDERR_STOP_ACTIVITY`, `STDERR_RESULT`, then `STDERR_LAST` | `daemon.cc::TunnelLogger`; `worker-protocol-connection.cc::processStderrReturn`; activity tags from `worker-protocol.hh` | activity field count ≤4; message ≤164; field string ≤153; `STDERR_NEXT` ≤145 | frame tags, declared maxima only; `STDERR_READ`, `STDERR_WRITE`, `STDERR_ERROR`, unknown field/tag reject |
| daemon → client | `AddTempRoot` / `IsValidPath` reply: `STDERR_LAST`, Boolean word | `daemon.cc::performOp` | Boolean only `0` or `1` | no body; any other value rejects |
| client → daemon | `AddToStore` (`7`): name string, content-address string, `StorePathSet`, repair Boolean, then `FramedSink` chunks and zero terminator | `RemoteStore::addCAToStore`, `daemon.cc::performOp(AddToStore)`; `FramedSink` / `FramedSource` in `src/libutil/include/nix/util/serialise.hh` | gate `>=1.25`; name ≤27, content address ≤11, references 0, chunk ≤502, total upload ≤502 | declared maxima and total only; chunk body streamed, never retained; terminator required |
| daemon → client | `AddToStore`: `STDERR_LAST`, then `ValidPathInfo`: path and `UnkeyedValidPathInfo` | `WorkerProto::Serialise<ValidPathInfo>` / `UnkeyedValidPathInfo`; `daemon.cc::performOp(AddToStore)` | path ≤153, optional deriver 0, SHA-256 and CA strings ≤64, references 0, signatures 0 | upstream `ValidPathInfo::maxSigs` is unlimited; P003C admits only observed zero |
| client → daemon | `QueryMissing` (`40`): count then `DerivedPath` strings | `RemoteStore::queryMissing`; `WorkerProto::Serialise<DerivedPath>`; `LengthPrefixedProtoHelper` | gate `>=1.19`; target count 1, string ≤157 | declared maxima only; a non-legacy path is still a string boundary here |
| daemon → client | `QueryMissing`: `STDERR_LAST`, three counted `StorePathSet`s, download and NAR-size words | `RemoteStore::queryMissing`; `daemon.cc::performOp(QueryMissing)`; `LengthPrefixedProtoHelper` | will-build count 1/path ≤153; substitute and unknown counts 0 | counts/lengths only; malformed collection rejects |
| client → daemon | `QueryPathInfo` (`26`): `StorePath` string | `BasicClientConnection::queryPathInfo`; `daemon.cc::performOp(QueryPathInfo)` | path ≤153 | declared maximum only |
| daemon → client | `QueryPathInfo`: `STDERR_LAST`, validity Boolean, then `UnkeyedValidPathInfo` iff valid | same `queryPathInfo` and `UnkeyedValidPathInfo` serializers | valid in this fixture; same path-info bounds above | invalid Boolean or missing typed fields rejects |
| client → daemon | `BuildPathsWithResults` (`46`): counted `DerivedPath` strings, `BuildMode` word | `RemoteStore::buildPathsWithResults`; `WorkerProto::Serialise<DerivedPath>` / `<BuildMode>` | gate `>=1.34`; count 1; string ≤157; mode `0..2` | no path body retained; other mode rejects |
| daemon → client | `BuildPathsWithResults`: `STDERR_LAST`, counted `KeyedBuildResult`: legacy derived-path string, status word, error string, `>=1.29` timing words, `>=1.37` optional CPU durations, `>=1.28` counted `DrvOutputs` key/realisation strings | `WorkerProto::Serialise<KeyedBuildResult>` / `<BuildResult>`; `common-protocol.cc`; `LengthPrefixedProtoHelper` | results 1; result path ≤157; status `0..14`; error 0; outputs 1; output id ≤75; realisation ≤196 | declared maxima only; invalid status/duration tag or collection shape rejects |

The common worker primitive (eight-byte little-endian words; padded strings)
is defined in `src/libutil/include/nix/util/serialise.hh` and
`src/libutil/serialise.cc`. `src/libstore/common-protocol.cc` supplies the
string, `StorePath`, optional-path, and signature serializers; generic vector
and set counts come from
`src/libstore/include/nix/store/length-prefixed-protocol-helper.hh`.

## Explicitly unsupported fixture flows

| Flow class | Fixture reachability | Observer behavior |
| --- | --- | --- |
| Worker operation outside the two inventories above | none | Fail closed before relaying the untyped body |
| Callback (`STDERR_READ` or `STDERR_WRITE`) | not observed in successful fixed fixtures | Fail closed before relaying the untyped callback body |
| Upload operation other than the inventoried `AddToStore` flow | none | Fail closed before relaying the untyped body |
| `STDERR_ERROR` or unknown activity/result frame | not observed in successful fixed fixtures | Fail closed before relaying the untyped frame body |
| Content-addressed build-specific flow beyond the classic fixture's `AddToStore` staging | unsupported for MVP | Fail closed pending a concrete fixture and typed inventory |

The typed observer relays only the messages listed above byte-for-byte at exact boundaries. It uses a fixed 4096-byte transfer buffer for declared string and upload bodies and retains only approved bounded metadata: protocol versions, operation/frame tags, declared lengths/counts, scalar classifications, and terminal-frame counts. It retains no feature, daemon version, option value, store path, activity text, callback body, upload body, derivation payload, secret, or raw transport chunk.

The classic fixture establishes one typed framed `AddToStore` upload with an observed 502-byte fixture envelope. This proves bounded streaming and byte transparency for that exact operation; it is not a general large-upload or production service limit. P003B may now capture trusted and untrusted classic-build acceptance traces through this typed observer. Content-addressed build compatibility remains deferred and unsupported until a concrete fixture, required operations, result semantics, and primary-source evidence are defined.

Future fixture changes must extend this inventory with exact serializer references, version conditions, bounded message types, retained metadata fields, and golden fixtures before the observer accepts the new flow.
