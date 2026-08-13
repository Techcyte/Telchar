# Requester disconnect policy

## Status

Accepted and implemented.

## Decision

Disconnect behavior is deployment configuration. Worker-protocol bytes and other requester-controlled input cannot select or override it.

The configured running-work policy is one of:

- `detach-and-finish` — default. Accepted running work continues without a requester. Telchar validates outputs, creates output roots and durable output leases, releases derivation and input resources, and retains verified outputs for the configured retention lifecycle.
- `cancel-running` — optional operator policy. Telchar cancels the active executor, reaps owned processes, detaches the requester, and releases request derivation and input resources. No successful result is written to the disconnected requester.

Unknown values fail daemon startup.

## Lifecycle table

| Lifecycle point | Disconnect behavior |
| --- | --- |
| Handshake or request decode | End the protocol session; retain no request state. |
| Input upload before complete validation and promotion | Abort the incomplete object; do not register a valid gateway-store path. |
| Accepted but not running | Detach the requester. Queue ownership and cancellation semantics are defined with durable queueing. |
| Running with `detach-and-finish` | Stop client-facing writes and continue execution, validation, output leasing, and bounded retention. |
| Running with `cancel-running` | Cancel and reap execution, detach, and release request resources. |
| Output collection and validation | Follow the configured running-work policy because the request still owns active execution lifecycle resources. |
| Result delivery after durable success | Preserve committed output roots and leases. Failure to deliver terminal bytes does not roll back successful durable completion. |

## Reattachment

A requester cannot reconnect to its original detached protocol session. A later equivalent request may join active work or reuse completed outputs, but it does not receive earlier live logs or resume the original byte stream.

## Security boundary

Only trusted deployment configuration selects the policy. Request fields, SSH commands, client environment, backend responses, and worker-protocol extensions cannot change it.

Telemetry records only the bounded policy value and lifecycle action. It does not record request, session, lease, owner, or store-path identifiers.

## Consequences

`detach-and-finish` requires executor ownership to outlive the frontend relay and requires bounded retention after verified completion. `cancel-running` requires bounded cancellation and process reaping. Both paths require durable attachment and lease transitions before cleanup is considered complete.
