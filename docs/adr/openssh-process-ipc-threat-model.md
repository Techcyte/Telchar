# OpenSSH Process and Local IPC Threat Model

**Status:** Accepted for the initial restricted-ingress prototype

## Scope and trust boundary

OpenSSH is the network-facing authentication and transport boundary. A per-connection forced command starts one unprivileged `telchar serve-stdio` frontend. The frontend owns only the worker-protocol stream and a bounded request attachment. It does not connect to PostgreSQL, access the gateway store, schedule work, select an executor, or perform administrative operations.

The single Telchar daemon owns admission, requester policy, durable state, gateway-store operations, scheduling, backend dispatch, reconciliation, and operational state. The daemon is the only process allowed to perform those privileged or shared-state operations. The frontend reaches it through a dedicated local IPC endpoint.

```text
remote client
  -- SSH host key + user public-key authentication --> OpenSSH
  -- authenticated connection --> forced-command frontend (unprivileged)
  -- authenticated local IPC + bounded envelope --> Telchar daemon
  -- PostgreSQL / gateway store / executor boundaries --> trusted infrastructure
```

## Trusted metadata sources

- OpenSSH authentication result is authoritative for whether the connection is authenticated.
- The authenticated public-key fingerprint comes from OpenSSH-controlled connection state, not client-supplied environment, command arguments, protocol fields, or arbitrary SSH variables.
- OpenSSH-controlled certificate metadata is authoritative only when certificate support is explicitly enabled and its source is captured and validated; certificate support is a separate prototype gate.
- `SSH_CONNECTION` is transport context supplied by OpenSSH and is retained only as bounded source-address context. It is not an authentication credential.
- The frontend passes metadata to the daemon only inside the versioned authenticated IPC envelope. The daemon treats the envelope as untrusted until local peer credentials and envelope integrity are checked.
- Worker-protocol payloads are build requests, not identity metadata.

## Local peer authentication

The daemon accepts IPC only from the expected local service identity using OS-enforced socket ownership and permissions plus peer credentials (`SO_PEERCRED` or the platform equivalent). The socket is created in a private runtime directory, is not reachable through a network listener, and is removed on shutdown. A frontend running as another user, a process that replaces the socket, and a client that sends forged metadata must be rejected before request attachment or shared-state access.

IPC messages are versioned, length-bounded, and carry a daemon-issued session/attachment identifier. The daemon binds the attachment to the authenticated local peer and refuses replay, unknown versions, oversized metadata, and mismatched session identifiers. T050/T051 define the concrete envelope and peer-authorization tests; this ADR defines their security obligations.

## Spoofing and abuse threats

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| Client supplies a fake fingerprint in an environment variable or command argument | Read identity only from OpenSSH-controlled state; ignore client-provided identity fields | OpenSSH configuration must not expose an attacker-controlled substitution |
| Frontend claims a different requester over IPC | Daemon authenticates local peer and validates an authenticated envelope bound to the session | A compromised service account can impersonate the frontend |
| Unprivileged process connects to daemon socket | Private socket directory, ownership/mode, peer-credential allowlist, and negative wrong-user fixture | Host root remains trusted |
| Client requests arbitrary shell command | `ForceCommand` invokes only the frontend; frontend does not interpret requested commands | Misconfiguration of `sshd_config` can bypass the intended entrypoint |
| Client allocates PTY or forwarding channels | Disable PTY, agent/X11 forwarding, and TCP forwarding in the restricted account configuration | OpenSSH configuration drift |
| Client injects worker-protocol identity fields | Protocol identity fields are ignored; identity is attached before protocol dispatch | Future protocol additions must preserve this rule |
| Client replays an IPC envelope | Daemon-issued session/attachment IDs, bounded lifetime, and one-time attachment state | Durable replay defense requires later request-state design |
| Frontend floods daemon or sends oversized metadata | IPC frame and field limits, per-session admission, and daemon-side validation before allocation | Resource limits are defined by T050/T052 |
| Frontend reads or modifies shared state directly | Separate privilege, filesystem permissions, and code boundary; frontend has no database/store credentials | A frontend process compromise can still consume its own process resources |
| Source address is treated as identity | Source address is audit/emergency context only; credential and quota subjects derive from authenticated identity | Network attribution can be affected by trusted proxy topology |
| Telemetry leaks credentials or unbounded request data | Use established `tracing`/OTLP path, bounded low-cardinality attributes, redaction policy, and spans at IPC boundary | Diagnostic detail remains intentionally limited |

## Security invariants

1. Authentication occurs before worker-protocol work is accepted.
2. No client-controlled value can select the requester credential, audit subject, or quota subject.
3. The frontend cannot reach PostgreSQL, the gateway store, or backend controls.
4. The daemon never trusts frontend metadata solely because it arrived over a local socket.
5. IPC failures are fail-closed and leave no partially attached request.
6. Boundary failures emit structured tracing events without placing sensitive or unbounded values in telemetry.

## Verification checklist

- [x] Frontend and daemon privileges are distinct.
- [x] Network and local IPC data flows are identified.
- [x] Trusted metadata sources and untrusted metadata sources are explicit.
- [x] Local peer authentication obligations are explicit.
- [x] Public-key, certificate, source-address, command, forwarding, replay, and resource-exhaustion spoofing threats have mitigations.
- [x] Telemetry boundary and redaction obligations are explicit.
- [x] Deferred concrete IPC envelope and peer tests are assigned to T050/T051 rather than silently implemented here.
