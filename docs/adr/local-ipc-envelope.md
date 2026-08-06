# Local IPC Envelope

**Status:** Accepted for the initial frontend/daemon boundary

The forced-command frontend sends one bounded, versioned envelope to the daemon before forwarding its worker-protocol stream on the same authenticated Unix connection. The envelope carries only authenticated requester metadata supplied by the OpenSSH-controlled frontend, a connection-scoped session ID, and an optional bounded error descriptor. Worker-protocol payloads remain outside the envelope.

## Wire contract

- Magic: `TIPC`.
- Version: little-endian `u16`; supported version is `1`.
- Strings: non-empty UTF-8, little-endian `u16` byte length.
- Audit subject, configured quota subject, and session ID: maximum 256 bytes each.
- Normalized credential ID and credential-ID quota fallback: maximum 1024 bytes each; the larger bound covers length-prefixed certificate identities whose authenticated components are individually bounded to 256 bytes.
- Error code: maximum 256 bytes.
- Error message: maximum 4096 bytes.
- Complete encoded envelope: maximum 16 KiB.
- Error flag: `0` absent or `1` followed by error code and message.
- Unknown versions, malformed UTF-8, empty strings, trailing bytes, truncation, and bounds violations fail closed before stream attachment.

The daemon must authenticate the local peer independently of this envelope. Envelope metadata is not trusted merely because it arrived over a local socket. The session ID is correlation metadata, not authorization material. Connection binding is defined by `local-ipc-frontend-attachment.md`: exactly one envelope and one worker session share the authenticated stream. Every encode/decode rejection uses the established `tracing` path with bounded reason fields and no requester values.

## Verification

`crates/telchar/tests/ipc_schema.rs` proves round-trip preservation, version rejection, bounded error data, and checked conversion of maximum valid normalized requester identities. Constants in `crates/telchar/src/ipc.rs` are the authoritative supported version and size bounds.
