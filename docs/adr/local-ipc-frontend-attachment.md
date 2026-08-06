# Local IPC Frontend Attachment

**Status:** Accepted for the initial frontend/daemon boundary

`serve-stdio` uses one local Unix stream per SSH connection. The frontend writes one length-prefixed TIPC envelope, then forwards the worker-protocol bytes on the same stream. The daemon accepts and authenticates the peer before decoding the envelope; after successful validation, it exposes the stream attachment without creating scheduler, database, or gateway-store state.

The length prefix is a little-endian `u32` and is rejected before allocation when greater than the 16 KiB envelope bound. The daemon records the kernel peer PID only as bounded connection telemetry/test evidence; PID is not requester identity and is not a durable authorization field. Stream bytes are not copied into an unbounded application buffer by the attachment API.

`crates/telchar/src/ipc.rs::IpcListener` implements listener acceptance, peer authorization, bounded envelope reception, and stream attachment. `crates/telchar/tests/ipc_frontend.rs` uses a real Unix listener and stream: the frontend envelope is decoded, `PING` is forwarded to the daemon, and `PONG` returns over the same connection. The test confirms both sides share the test process PID and no scheduler/database behavior exists in the boundary.
