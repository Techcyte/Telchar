# Local IPC Stream Buffering

**Status:** Accepted for the initial frontend/daemon boundary

Frontend-to-daemon stream forwarding uses one fixed 16 KiB stack buffer per relay direction. The relay reads at most that capacity, writes the received bytes before reading more, and therefore applies kernel/socket backpressure when the daemon or client is slow. It does not collect the protocol stream or allocate based on peer-provided lengths. Request forwarding always half-closes the daemon write side when standard input ends or the request relay fails. Response EOF lets the foreground frontend exit without waiting indefinitely for a request thread blocked on client input; process exit cancels that thread.

`crates/telchar/src/ipc.rs::relay_bounded` reports the observed maximum buffered bytes for test evidence and emits start/completion tracing spans/events with only the fixed bound and low-cardinality completion state. Production frontend relay failures emit only bounded reason classifications. Raw requester values, protocol bytes, socket paths, UIDs, PIDs, and error bodies are excluded. Relay failures leave no retry queue or unbounded retained payload.

`crates/telchar/tests/ipc_buffer.rs` uses real Unix socket pairs. A sender writes 32 times the configured buffer while the daemon reader initially pauses; the complete payload arrives byte-for-byte and observed maximum buffered bytes equals the configured 16 KiB bound. Separate-process frontend tests prove response completion terminates the frontend and request EOF reaches the daemon.
