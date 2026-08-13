# Validated NAR promotion

**Status:** Accepted and implemented

## Decision

Telchar validates staged NAR data against independently declared metadata, then imports it through the typed Nix worker-protocol `AddToStoreNar` operation.

Production store access is pure Rust and daemon-protocol based. Telchar does not use the Nix C++ ABI, a private helper, `nix-store --import`, or a caller-selected store endpoint.

## Validation order

1. Parse exactly one bounded raw NAR while computing its SHA-256 hash and byte size.
2. Compare that result with the declared NAR metadata.
3. Validate the declared store path, references, optional deriver, and supported content-address fields.
4. Require every non-self reference to be valid in the configured gateway store.
5. Send explicit normalized metadata and the bounded NAR stream with `AddToStoreNar`.
6. Query the authoritative store and require the registered metadata to match.
7. Remove staging state on success or failure.

No authoritative success is reported before the final store query passes.

## Failure semantics

Malformed NAR data, mismatched hashes or sizes, invalid paths, unsupported metadata, missing references, daemon errors, interrupted streams, or post-import metadata disagreement fail closed. A helper exit code or partially written store data is never treated as success.

## Rejected alternatives

### `nix-store --import`

Its export envelope does not provide the independent expected NAR identity required at Telchar's validation boundary.

### `nix store add`

It computes a content-addressed destination rather than registering the declared classic input-addressed path and metadata.

### Nix C++ or C APIs

They add ABI and packaging coupling and are unnecessary now that the required worker-protocol operation is implemented directly.
