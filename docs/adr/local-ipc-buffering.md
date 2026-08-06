# Local IPC Stream Buffering

**Status:** Accepted for the initial frontend/daemon boundary

Frontend-to-daemon stream forwarding uses one fixed 16 KiB stack buffer per relay direction. The relay reads at most that capacity, writes the received bytes before reading more, and therefore applies kernel/socket backpressure when the daemon or client is slow. It does not collect the protocol stream or allocate based on peer-provided lengths.

`crates/telchar/src/ipc.rs::relay_bounded` reports the observed maximum buffered bytes for test evidence and emits start/completion tracing spans/events with only the fixed bound and low-cardinality completion state. Relay failures return the underlying I/O error and leave no retry queue or unbounded retained payload.

`crates/telchar/tests/ipc_buffer.rs` uses real Unix socket pairs. A sender writes 32 times the configured buffer while the daemon reader initially pauses; the complete payload arrives byte-for-byte and observed maximum buffered bytes equals the configured 16 KiB bound.
