# Typed fixture-flow inventory

## Scope and evidence

This inventory covers every protocol flow reachable from the current compatibility fixtures at pinned Nix 2.34.7, source revision `2c6d06e9387cf58167cb5a7ab91cee7333d8d17c`.

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

`SetOptions` is the only fixture-reachable operation. Its exact request boundary begins at its typed operation word; the next message cannot begin until all twelve fixed words, override count, and each declared name/value pair have been consumed. The observer must retain only operation code, negotiated version, frame kind, declared string lengths, override count, and terminal frame kind. It must retain no feature, daemon-version, override-name, or override-value body.

## Explicitly unsupported fixture flows

| Flow class | Fixture reachability | Observer behavior |
| --- | --- | --- |
| Worker operation other than `SetOptions` | none | Fail closed before relaying the untyped body |
| Callback (`STDERR_READ` or `STDERR_WRITE`) | none | Fail closed before relaying the untyped body |
| Upload (`AddToStore`, `AddToStoreNar`, `AddMultipleToStore`) | none | Fail closed before relaying the untyped body |
| Activity/error/result frame other than `STDERR_LAST` | none | Fail closed before relaying the untyped body |

The typed observer relays only these listed messages byte-for-byte at their exact boundaries. It uses a fixed 4096-byte transfer buffer for declared string bodies and retains only protocol versions, operation tag, declared feature/daemon/override string lengths, override count, and terminal-frame count. It retains no feature, daemon version, option name, option value, callback body, upload body, secret, or raw chunk.

Large-upload coverage is intentionally absent: no current concrete fixture establishes a typed upload boundary. A synthetic upload stream is not protocol acceptance evidence. P003B remains blocked pending explicit trusted-classic/untrusted-classic fixture-definition tasks and a matching inventory/parser extension. Content-addressed compatibility is deferred from the initial gate and unsupported until a concrete fixture, required operations, result semantics, and primary-source evidence are defined.

Future fixture changes must extend this inventory with exact serializer references, version conditions, bounded message types, retained metadata fields, and golden fixtures before the observer accepts the new flow.
