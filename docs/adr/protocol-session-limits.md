# Protocol Session Resource Limits

## Context

The Nix worker protocol is stateful and has no generic message envelope. Per-field limits prevent one declared string or collection from allocating without bound, but they do not bound the total metadata retained while decoding a typed request. A stalled client can also hold a frontend indefinitely while sending only part of a typed message.

A monotonically increasing lifetime byte budget is not suitable: every sufficiently long valid session would eventually exhaust it even after previous values were released. Streamed NAR, derivation, and upload bodies also require transfer admission rather than metadata allocation accounting.

## Decision

Telchar passes one typed `ProtocolSessionLimits` value to each protocol session. The initial defaults are:

- 16 MiB maximum concurrently retained decoded metadata.
- 30 seconds maximum idle time while an incomplete typed protocol message or frame is being read.

The allocation budget measures heap capacity requested for decoded strings, byte strings, collections, and nested metadata. A decoder charges the requested capacity before allocation using checked arithmetic. Charge is released when the value is dropped, forwarded, or otherwise no longer retained by the session. Fixed-size stack fields are excluded.

Streamed payload bodies are excluded from the metadata budget. NAR data, derivation payloads, and framed uploads remain bounded-memory streams and are governed by transfer admission and operation-specific limits. Declared metadata that would exceed the remaining budget is rejected before allocation.

The idle deadline applies only while a handshake field, request, response, callback, activity frame, error frame, result frame, or upload frame is incomplete. It resets whenever protocol input makes forward progress. A session waiting at a complete typed message boundary does not expire under this rule.

`nix-worker-protocol` remains generic over synchronous `Read` and `Write` and does not own clocks, threads, polling, sockets, or process supervision. Timeout enforcement belongs to the Telchar transport layer. `serve-stdio` uses the default `ProtocolSessionLimits`; tests may inject shorter durations through the same typed configuration. A timeout returns `io::ErrorKind::TimedOut`, closes the session cleanly, releases resources, and emits bounded low-cardinality telemetry without protocol bodies or requester secrets.

The Linux stdio transport may use readiness polling around its input descriptor. It must not rely on a detached or unkillable blocking reader thread.

## Consequences

The allocation rule bounds live decoded metadata without penalizing long-lived sessions that release previous messages. Streaming remains independent from metadata accounting, so large legitimate payloads do not require equivalent heap capacity.

The timeout protects incomplete protocol reads without disconnecting a healthy connection merely because no operation is currently active. Future service configuration may expose these two fields, but all call sites continue to use the typed limits object rather than independent constants or environment-specific behavior.
