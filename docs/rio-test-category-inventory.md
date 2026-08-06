# Rio-informed protocol test-category inventory

## Scope and source boundary

This checklist records test-category observations from rio-build revision
`59e832144d67c1b1973272ef394ffc6ef2629f4b`, identified in
`docs/rio-build-reference.md`. The review used repository-level architecture,
fuzzing, VM-test, and observability category descriptions only. It did not read,
copy, translate, or mechanically adapt Rio implementation code or test bodies.

The categories below are not wire-behavior evidence. Telchar protocol behavior
continues to require captured pinned-Nix traffic and primary Nix serializers as
recorded in `docs/protocol-evidence-inventory.md` and
`docs/protocol-fixture-flow-inventory.md`.

## Reference-to-test-category checklist

| Rio category observation | Current Telchar coverage | Decision | Rationale and follow-up |
| --- | --- | --- | --- |
| Wire primitive fuzzing | `crates/nix-worker-protocol/fuzz/fuzz_targets/primitive_framing.rs`, bounded primitive property tests | Adopted | Existing deterministic fuzz smoke and property tests cover malformed words, padded strings, truncation, and bounds. Keep corpus regression work at T258. |
| Real Nix client VM scenarios | reusable NixOS topology, pinned real-client handshake, classic trusted/untrusted fixture tests | Adopted | Current tests exercise the pinned client and captured fixture flows. Gate 1 T045 will consolidate malformed, oversized, unsupported, and unknown-input proof. |
| End-to-end protocol boundary with a real client | `stdio_handshake.rs`, `operation_dispatch.rs` | Adopted | T041 adds live `SetOptions` acceptance, exact stdout frame assertion, local stderr telemetry, and OTLP log export. Fixture-only parsers remain insufficient for acceptance. |
| Structured observability | `tracing`, OTLP collector tests, telemetry contract | Adopted | The T041A stdout policy keeps binary protocol bytes separate from local telemetry, while OTLP log/metric/trace export remains tested. |
| Malformed and resource-exhaustion inputs | size/truncation tests, allocation budget, idle-timeout tests, primitive fuzz target | Adopted | Current bounded primitive/session coverage provides baseline. T044 owns outbound/inbound structured-frame budgets; do not duplicate that behavior here. |
| Cross-version or alternate-client compatibility | one pinned stock Nix 2.34.7 matrix cell | Deferred | Telchar supports exactly one pinned stock Nix version. Additional Nix releases and Lix require concrete traffic capture, primary-source inventory, and a matrix expansion. |
| Content-addressed derivation behavior | explicitly unsupported fixture flow | Deferred | Classic input-addressed fixture evidence cannot establish CA semantics. Require a concrete CA fixture, exact serializers, result semantics, and acceptance proof before support. |
| Scheduler DAG and placement policy | outside `nix-worker-protocol` crate boundary | Rejected | Telchar design delegates scheduling work to later domain tasks; importing Rio scheduler categories here would blur the protocol crate boundary. |
| Chunked CAS, FUSE stores, Kubernetes builders, and per-build overlays | outside current protocol behavior and initial deployment constraints | Rejected | These are Rio-specific execution/storage architectures, not protocol test categories for the initial Telchar scope. |
| Multi-tenant authentication and per-tenant isolation | deferred by Telchar trusted shared-store domain | Rejected | Initial Telchar explicitly supports one mutually trusted client-store domain. Identity/quota work occurs only after ingress design proof. |

## Review result

The review identified no untracked protocol test category requiring implementation
before T043. The adopted categories map to existing tests or to already-owned
future tasks T044, T045, and T258. Deferred categories remain unsupported until
the listed evidence prerequisites are satisfied.
