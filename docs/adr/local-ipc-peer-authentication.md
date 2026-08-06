# Local IPC Peer Authentication

**Status:** Accepted for the initial Linux daemon boundary

The daemon authenticates each accepted Unix-stream peer before decoding or attaching an IPC session. On Linux it reads `SO_PEERCRED` through `rustix` and accepts only the configured expected service UID. A peer with any other UID is rejected with `PermissionDenied`; peer PID and GID are not used as requester identity. The envelope remains independently version- and size-validated.

The production socket must live in a private runtime directory owned by the daemon service account, with filesystem permissions restricting traversal and connection. Peer credentials are the authoritative connection check; filesystem permissions are defense in depth. Authorization failures emit bounded `tracing` events (`ipc.peer.rejected`) without UID, PID, requester metadata, or socket content. Successful authorization emits a low-cardinality debug event.

`crates/telchar/src/ipc.rs::authorize_peer` implements the check. `crates/telchar/tests/ipc_auth.rs` uses real Unix socket pairs and the kernel-reported current UID: the expected UID is accepted and a wrong UID is denied. The implementation is Linux-specific because the deployment baseline and `SO_PEERCRED` contract are Linux-specific; other platforms require a separately reviewed credential mechanism before support.
