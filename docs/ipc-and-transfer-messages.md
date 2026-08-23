# IPC and transfer messages

This document records message shapes that Telchar executes today. Statements under **Executable authority** are enforced by production code and tests. Statements under **Observed authority** describe behavior exercised against supported Nix clients or the deployed Nomad topology. Statements under **Inference** are explanatory and are not protocol guarantees.

## Local frontend IPC

### Purpose

The SSH frontend authenticates the external requester, constructs one bounded identity envelope, and then relays the Nix worker-protocol byte stream over a Unix socket to the Telchar daemon. The local IPC layer does not reinterpret Nix worker operations after the envelope.

### Executable authority

The daemon accepts a Unix stream only when Linux `SO_PEERCRED` reports the configured UID. PID and GID are not authorization inputs. On non-Linux platforms peer authorization fails as unsupported.

The frontend sends:

```text
u32 little-endian envelope length
TIPC envelope bytes
Nix worker-protocol byte stream
```

The envelope length may not exceed `16 KiB`. The envelope is:

```text
4 bytes   magic: "TIPC"
u16 LE    version: 1
string    credential_id
string    audit_subject
string    quota_subject
string    session_id
u8        error-present flag
[if 1]
  string  error code
  string  error message
```

Each string is encoded as:

```text
u16 LE byte length
UTF-8 bytes
```

Bounds:

| Field | Maximum bytes |
| --- | ---: |
| envelope | 16,384 |
| `credential_id` | 1,024 |
| `audit_subject` | 256 |
| `quota_subject` | 1,024 |
| `session_id` | 256 |
| error code | 256 |
| error message | 4,096 |

Strings must be non-empty. Wrong magic, unsupported version, invalid error flag, trailing bytes, truncation, invalid UTF-8, and bound violations fail closed. The daemon applies a five-second default envelope-read timeout, then removes that timeout before handing the stream to the Nix session.

After acceptance, relay uses a fixed `16 KiB` buffer, flushes each read, and half-closes the destination write side at EOF. No complete Nix request, response, NAR, or session stream is retained in memory by this relay.

### Observed authority

The deployed SSH ingress and gateway run as UID `995`, allowing exact UID authorization across Nomad tasks sharing the allocation Unix socket. Production qualification showed stock Nix reaching the daemon through this attachment.

### Inference

`credential_id`, `audit_subject`, and `quota_subject` separate credential provenance, audit attribution, and quota identity. Their policy meaning belongs to the authenticated frontend and daemon identity model; the wire encoding itself does not establish that trust.

## Nomad allocation transfer

### Transport and framing

The allocation worker opens a WebSocket connection to the gateway callback endpoint using subprotocol `telchar-nomad-transfer-v1`. Every application message is binary and contains one TLNW frame.

Executable frame encoding:

```text
4 bytes   magic: "TLNW"
u16 BE    protocol version: 1
u16 BE    frame kind
u32 BE    metadata byte length
u32 BE    payload byte length
bytes     JSON metadata
bytes     binary payload
```

Metadata and payload bounds are phase-specific. JSON metadata uses strict structures with unknown fields rejected. Wrong magic, version, kind, lengths, direction, or phase fails closed.

### Message sequence

```text
worker  → gateway  Authenticate

gateway → worker   InputManifest
worker  → gateway  ValidPaths
worker  → gateway  InputRequest
gateway → worker   InputNar ...

worker  → gateway  BuildStarted
worker  → gateway  LogChunk ...
worker  → gateway  OutputMetadata
worker  → gateway  OutputNar ...
gateway → worker   OutputReceipt
worker  → gateway  BuildResult
```

`LogChunk` and output pairs repeat as permitted by session state. Invalid reordering or direction is rejected by the transfer state machine.

### Nomad callback authentication

This section is specific to callbacks from `telchar-nomad-worker` running inside a Nomad allocation. `Authenticate` metadata contains:

- backend name;
- namespace;
- deterministic job ID;
- allocation ID;
- task name;
- shared-build digest;
- either a workload-identity JWT or HMAC capability proof.

**Nomad workload identity** uses the JWT that Nomad issues to the allocation task. Telchar validates its RS256 signature against the configured Nomad JWKS, plus exact audience, timing, namespace, job, allocation, and task claims. Issuer verification is optional configuration.

The alternative HMAC mode is a Telchar callback capability scoped to the same execution identity. Authentication secrets, JWTs, and capabilities are not stored in PostgreSQL and must not be logged.

### Input authority

`InputManifest` binds:

- derivation path;
- exact build specification;
- complete admitted input closure metadata;
- exact expected outputs.

Each path entry contains path, NAR hash, NAR size, references, optional deriver, and optional content address. The build specification contains exact outputs and fixed-output authority, input sources, system, required features, builder, arguments, and environment.

The worker reports `ValidPaths` from its allocation-side store, then sends `InputRequest` containing exactly the unresolved admitted set. The gateway rejects missing, duplicate, extra, or foreign request membership. Ordinary allocation-side Nix daemon configuration may make paths valid before the callback starts, but the current transfer session does not invoke substitution or realization between `ValidPaths` and `InputRequest`.

Each `InputNar` carries `NarMetadata` plus raw NAR payload:

- path;
- NAR hash;
- total NAR size;
- byte offset;
- final-chunk marker.

Chunks are ordered and non-interleaved. Empty chunks, gaps, overlap, early completion, late completion, path changes, hash changes, individual limits, and aggregate limits fail closed. The gateway now records bounded aggregate input-transfer diagnostics: manifest path count, requested/transferred path count, requested/transferred NAR bytes, and total phase duration. It does not log NAR contents.

### Build, logs, and outputs

`BuildStarted` identifies the admitted derivation. `LogChunk` metadata carries a monotonic sequence number; payload contains bounded log bytes. The worker sends these frames and the gateway validates their phase and size, but the current Nomad callback path does not forward them to attached Nix clients. Logs are not stored in PostgreSQL.

For each expected output, the worker sends `OutputMetadata` followed by ordered `OutputNar` chunks. The gateway validates declared identity and authority, NAR structure and hash, references, exact path set, transfer bounds, and gateway registration. The current gateway sends an accepted `OutputReceipt` only after successful import; rejection terminates the callback. The protocol model reserves rejected receipts, but production does not emit them.

`BuildResult` is terminal. `Built` requires every expected output to have an accepted receipt and no diagnostic. `Failed` may carry one bounded diagnostic. Allocation completion alone is not build success.

### Observed authority

Production qualification exercised a `2,960`-path manifest where only two unresolved paths were transferred, followed by one target `BuildDerivation`, exact output return, gateway validation, retention, and successful stock-Nix result handling. Nomad task restarts and group rescheduling were both disabled and observed at zero.

### Inference

The transfer resembles selective cache filling, but Telchar is not a binary cache. The manifest is admission authority for one execution attempt; gateway validation and the Nix distributed-builder relationship remain authoritative for the returned build result.

## Durable control-plane boundary

PostgreSQL stores build identity, backend selection, deterministic backend execution ID, expected outputs, admitted build specification, recovery state, and terminal result metadata. Callback replay records separately store bounded allocation and nonce identity. It does not store NAR bodies, secret credential material, capabilities, JWTs, signatures, or log streams.

The current worker does not reconnect a failed callback. A callback or transfer failure terminates the attempt; Telchar must not submit another job, migrate an in-flight build, or repeat `BuildDerivation` automatically.

## Executable references

- `crates/telchar/src/service/ipc.rs`
- `crates/telchar/src/service/session/mod.rs`
- `crates/telchar/src/nomad/protocol.rs`
- `crates/telchar/src/nomad/callback_service.rs`
- `crates/telchar-nomad-worker/src/lib.rs`
- `crates/telchar/tests/ipc_schema.rs`
- `crates/telchar/tests/ipc_auth.rs`
- `crates/telchar/tests/ipc_buffer.rs`
- `crates/telchar/tests/nomad_transfer_protocol.rs`
- `crates/telchar/tests/nomad_transfer_session.rs`
- `crates/telchar/tests/nomad_input_session.rs`
- `crates/telchar/tests/nomad_output_session.rs`
