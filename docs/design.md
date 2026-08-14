# Architecture

Telchar sits between stock Nix clients and a fleet of Nix executors. It preserves Nix's remote-builder interface while centralizing admission, queueing, backend selection, recovery, and output validation.

```text
stock Nix client
  │ ssh-ng / worker protocol
  ▼
OpenSSH forced-command frontend
  │ authenticated Unix socket
  ▼
Telchar daemon
  ├─ PostgreSQL coordination
  ├─ gateway Nix store
  └─ backend
      ├─ local Nix
      ├─ static SSH
      └─ Nomad allocation worker
```

## Responsibilities

Nix still evaluates expressions and decides when a derivation is ready. Telchar sees only the build operations submitted by the client daemon; it does not reconstruct the evaluation graph or act as a CI scheduler.

Telchar owns:

- authenticated requester and quota identity;
- build validation and resource admission;
- subject-fair queueing and backend permits;
- duplicate suppression for equivalent builds;
- durable execution and recovery state;
- input and output movement through the gateway store;
- backend submission, monitoring, and exact-target cancellation;
- output validation and the final Nix `BuildResult`;
- bounded telemetry and live log delivery.

The infrastructure scheduler owns machine placement and autoscaling. Nix stores and substituters own content storage. OpenSSH owns network authentication. PostgreSQL owns durable control-plane state.

## Process model

OpenSSH starts `telchar serve-stdio` as a restricted forced command for each client connection. The frontend attaches authenticated identity to the worker-protocol stream and forwards it over a private Unix socket. It has no database access and does not schedule work.

One `telchar daemon` process owns the socket, scheduling, gateway-store access, backends, callback service, recovery monitors, and maintenance work. Before becoming ready it takes the fixed PostgreSQL advisory lock `0x5445_4c43_4841_5202` on a dedicated lifetime connection. The lock connection performs no ordinary transactions.

Lock contention rejects startup before listeners or backend side effects begin. Losing the connection permanently fences the daemon: it closes admission, prevents new mutations and submissions, joins bounded service work, and exits. Reconnecting does not restore ownership. PostgreSQL releases the lock after connection loss or process death, allowing a replacement daemon to perform normal recovery.

This is single-active process exclusion, not high availability. A standby design would need leadership epochs, callback routing, dispatch fencing, and shared or replicated gateway-store authority.

## Build lifecycle

1. OpenSSH authenticates the client and starts the frontend.
2. The daemon negotiates the supported worker protocol and validates the request.
3. Telchar checks system, features, quotas, transfer limits, and disk reserve.
4. Required inputs are made valid in the gateway store.
5. Equivalent requests join one durable shared build.
6. The leader waits in a round-robin queue across quota subjects, FIFO within a subject.
7. Telchar selects the first configured compatible backend and acquires its permit.
8. The selected backend executes independently of requester attachment.
9. Every declared output is returned to the gateway, validated, imported, and confirmed in the store.
10. Telchar records one terminal result and replies using the normal Nix worker protocol.

One equivalent derivation has at most one active shared execution. The derivation path and a digest of the admitted specification protect request equivalence. Shared builds move through `claimed`, `running`, `collecting`, `succeeded`, or `failed`.

The first requester owns shared-build quota until terminal completion. Followers use the same execution and consume no extra execution slot or backend permit. Queue and active limits are transactional. Backend capacity is a separate gate, so an admitted build may wait for its selected backend permit.

Requester disconnect behavior is operator configuration:

- `detach-and-finish` is the default and leaves admitted work running;
- `cancel-running` cancels and reaps the owned execution.

Client bytes cannot select the policy. A disconnected requester cannot resume its original protocol stream. A later equivalent request may join active work or reuse completed outputs, but receives neither earlier logs nor the old byte stream.

A failed shared build is terminal. Telchar does not automatically retry it. A later independent request may create replacement work after checking the gateway store.

## Durable state and recovery

PostgreSQL stores bounded request identity, attachments, shared-build state, admitted build specifications, attempts, backend identity, transfer progress, and terminal metadata. It does not store NAR bodies, credentials, capabilities, signatures, or build logs.

Recovery first checks the exact expected outputs in the gateway store. If they are valid, they win regardless of the previous transient state. Otherwise recovery follows the persisted backend identity:

- local and static SSH execution are output-only; missing outputs fail closed;
- Nomad execution is adoptable only through the original backend, namespace, and persisted job identity.

A compatible backend is fungible only before dispatch. In-flight work is never migrated or blindly resubmitted.

## Gateway store

The gateway Nix store is durable authority for admitted inputs, returned outputs, retention, and restart recovery. Telchar accesses it through typed worker-protocol operations rather than the Nix C++ ABI or shell commands.

NAR import follows a fixed validation order:

1. parse one bounded NAR while computing its hash and size;
2. compare those values with independently declared metadata;
3. validate the path, references, deriver, and supported content-address fields;
4. require non-self references to exist in the gateway store;
5. stream the normalized metadata and NAR with `AddToStoreNar`;
6. query the store and require the registered metadata to match.

Malformed data, metadata disagreement, missing references, interruption, or daemon failure fails closed. Telchar does not use `nix-store --import`, `nix store add`, or the Nix C++ ABI for production import.

Transfers are streamed with bounded memory. Store leases and GC roots keep paths alive while requests, active executions, transfers, or configured output retention still require them. Telchar controls eligibility; Nix garbage collection performs deletion.

Classic input-addressed output validation proves transport and store consistency with a trusted executor. Fixed-output validation additionally requires the registered Nix content address to match the admitted method, algorithm, and digest. It is not cryptographic proof that the executor built honestly.

After durable shared-build leadership and subject admission, Telchar may ask the gateway Nix daemon to `EnsurePath` expected outputs before acquiring backend capacity. Complete hits use the same validation and retention path; misses fall through to execution. Optional cache publication runs only after durable success and cannot change the Nix result.

## Backends

### Local

Runs `BuildDerivation` through the configured gateway-side Nix daemon. It is the reference backend and has connection-bound cancellation.

### Static SSH

Uses a configured SSH destination, identity, and pinned host-key file. Recovery reconnects only to that exact backend and succeeds only when every expected output can be imported and validated.

### Nomad

Submits a deterministic batch job and persists its identity before monitoring. A packaged allocation worker connects back to Telchar, authenticates, resolves the admitted input closure, builds through its configured Nix store, streams live logs, and returns exact outputs.

The callback uses WebSocket transport with the `telchar-nomad-transfer-v1` subprotocol and typed TLNW messages. Authentication is workload identity or an HMAC capability. Public `wss://` endpoints require an operator-managed TLS terminator; Telchar itself listens with plaintext WebSocket.

See [Nomad backend](nomad.md).

## Security boundary

Telchar assumes authenticated clients, executor hosts, their Nix daemons, PostgreSQL, and the gateway host are trusted members of one organizational store domain. A client that knows a store path may be able to query it. Store-path opacity is not authorization.

Build payloads remain untrusted and must run in Nix sandboxes. Client bytes cannot select backend names, credentials, stores, Nomad clusters, drivers, cache policy, quotas, or deployment settings.

OpenSSH ingress exposes no shell, PTY, forwarding, or arbitrary command execution. Secrets are delivered through protected files or the deployment's secret mechanism, never inline in generated Nix configuration.

Hostile multi-tenancy needs a separate store, cache, log, backend, and recovery isolation design.

## Protocol boundary

The `nix-worker-protocol` crate owns bounded wire primitives, negotiation, typed operations, activity and error frames, build results, fixtures, property tests, and fuzz targets. It may emit `tracing` instrumentation but contains no Telchar identity, scheduling, persistence, backend, service configuration, or OpenTelemetry exporter policy. Telchar may depend on the protocol crate; the reverse dependency is forbidden. `scripts/check-protocol-boundary.sh` enforces that direction.

Unknown or unsupported operations fail closed because the worker protocol has no generic envelope that can safely skip arbitrary messages. Compatibility claims require typed coverage and real Nix fixtures, not only a matching protocol number.

## Non-goals

Telchar does not:

- evaluate flakes or expressions;
- replace client `nix-daemon` processes;
- run CI pipelines, jobsets, or arbitrary commands;
- provision compute or implement autoscaling;
- provide a binary cache or log product;
- provide interactive builder shells;
- migrate active builds between backends;
- perform generic automatic retries;
- support active/active scheduling;
- provide hostile multi-tenant isolation;
- terminate TLS.

Future work is tracked in the [roadmap](roadmap.md).
