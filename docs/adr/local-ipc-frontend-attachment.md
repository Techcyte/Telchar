# Local IPC Frontend Attachment

**Status:** Accepted for the initial frontend/daemon boundary

`serve-stdio` opens exactly one local Unix stream per SSH connection. The frontend writes one length-prefixed TIPC envelope and then forwards worker-protocol bytes over that same stream. The daemon authenticates the socket peer before reading the envelope and validates the complete envelope before accepting worker-protocol input.

The authenticated Unix connection is the attachment. Session identity is scoped to that connection and is consumed when the daemon accepts the envelope. There is no separately issued attachment token, second attachment connection, detached attachment registry, or replayable attachment identifier. Closing either endpoint invalidates the session. A second connection carrying the same session value is a separate request and receives no authority from the earlier connection.

This design provides the required binding directly:

- `SO_PEERCRED` binds the frontend process identity to the accepted socket.
- The bounded envelope and worker stream share one ordered byte stream, so protocol bytes cannot attach to another envelope or peer.
- The daemon processes at most one envelope and one worker session per accepted connection.
- Unknown versions, malformed or oversized envelopes, frontend-reported envelope errors, partial-envelope timeout, peer-authentication failure, and disconnect before validation fail closed without entering worker-protocol handling.

The length prefix is a little-endian `u32` and is rejected before allocation when greater than the 16 KiB envelope bound. Persistent daemon peer-authentication failures reject only that connection and do not terminate listener availability. Envelope reception has a fixed incomplete-envelope deadline. Protocol relay uses fixed-size buffers and kernel socket backpressure; neither side retains unbounded protocol bytes. The daemon records only bounded lifecycle classifications and may record the kernel peer PID for test evidence. PID is not requester identity or durable authorization state.

A daemon-issued token would add issuance, expiry, storage, replay, and mismatch state without strengthening this one-connection boundary. Such a token becomes necessary only if a future design intentionally separates metadata and worker bytes across connections. That design is out of scope and requires a separate decision.

Acceptance requires distinct frontend and daemon OS processes, authenticated peer credentials, a real Nix worker handshake through `serve-stdio`, malformed, failed, and stalled envelope rejection, byte-transparent stdout, bounded buffering and concurrent-session admission, private pre-existing runtime-directory validation, and cleanup when either endpoint disconnects. Same-process threads and `PING`/`PONG` fixtures are not acceptance evidence.
