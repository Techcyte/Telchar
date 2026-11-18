# Client upload verification

## Status

Implementation in progress; qualification pending.

## Context

Authenticated clients share a trusted gateway store domain. NAR integrity proves consistency with declared metadata but does not prove an input-addressed path resulted from executing its derivation. An authenticated client can otherwise upload its own bytes under an input-addressed path. Operators need optional attribution and explicit trust controls without modifying stock Nix clients or applying client policy to executor output collection.

## Decision

Keep audit and upload verification independently default off. Enforce verification at client `AddMultipleToStore` before staging/import, not in shared store promotion. Existing executor, callback and recovery validation remain authoritative for executor results.

Reconstruct the canonical Nix fingerprint and verify Ed25519 signatures against operator-owned registry key bytes. Count distinct key material rather than signature instances or labels. Signature identity, revocation and validity come only from the registry. Optional identity enforcement requires a valid matching signer in addition to the configured threshold. A different valid identity alone is not a reason to reject when another signer satisfies identity enforcement.

Union static, file and command sources without choosing a winner for ambiguous detailed records. Dynamic source failures retain their last-known-good snapshots. Explicit revocation remains auditable; conflicts quarantine names. Bound resolver input/output, execution time and refresh concurrency. Keep credentials and arbitrary helper diagnostics out of client errors and audit records.

Content-address exemption is conditional on existing Nix daemon content/path validation. Merely declaring a content address must never authorize arbitrary path/content combinations. Valid signatures also leave NAR hash, size and reference validation intact. Already-valid paths undergo client policy before an accepted no-op import.

An optional external policy is deny-only: built-in signature rejection cannot be overridden. It belongs after built-in checks, skips exempt content-addressed paths and must fail closed on invocation/response failures. Policy implementation and qualification are separate from the registry/signature gate.

## Consequences

Developer keys provide attribution, not build provenance. Restricting trusted keys to controlled builders/upstreams narrows upload trust; a developer-writable cache sharing a trusted signing key can defeat that restriction. This remains one shared store domain, not hostile multi-tenancy.

Stock client uploads carry source-store signatures; Telchar adds no worker messages, upload retries or automatic rebuild fallback. Rejection can fail a stock client's request instead of causing a remote rebuild. Compatibility claims require real-client evidence.

Audit remains a bounded best-effort signal, not a lossless compliance ledger. Resolver last-known-good retention can preserve previous trust during outages. Operators must account for that tradeoff and use explicit revocation and controlled credential distribution.
