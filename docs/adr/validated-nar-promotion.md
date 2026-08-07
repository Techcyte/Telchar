# Validated NAR Promotion

**Status:** Accepted for the initial Gate 3 implementation boundary

## Decision

Telchar promotes a staged NAR through Nix's typed `Store::addToStore(const ValidPathInfo &, Source &, ...)` operation. When the configured store endpoint is daemon-backed, Nix serializes this call as worker operation `AddToStoreNar` (`39`) with explicit path metadata and a framed raw-NAR stream.

Telchar must not use legacy `nix-store --import` as the production promotion boundary. That format derives the NAR hash from the same untrusted archive and therefore has no independent expected hash against which to validate content.

The initial adapter may use a small helper linked against the flake-pinned Nix C++ `nix-store` library. The stable Nix C API does not expose raw-NAR registration with `ValidPathInfo`. The helper is an implementation detail of the typed gateway-store adapter, not a deployment mode or independently exposed service.

## Validation and promotion order

The daemon performs these steps in order:

1. Parse exactly one raw NAR into daemon-owned staging while computing SHA-256 and byte size.
2. Compare the computed hash and size with independently declared metadata.
3. Parse the declared path, references, and optional deriver against the configured store directory.
4. For the initial classic input-addressed subset, require:
   - SHA-256 NAR hash;
   - absent content address;
   - absent signatures;
   - `ultimate = false`;
   - `repair = false`;
   - a bounded, duplicate-free reference set;
   - every non-self reference already valid in the gateway store;
   - an optional deriver that is a syntactically valid `.drv` store path.
5. Rewind the staged NAR and call `Store::addToStore` with explicit `ValidPathInfo` and signature checking disabled only because Telchar has already normalized the authenticated, supported metadata subset. The helper must not accept caller-controlled store endpoints or policy flags.
6. Query the authoritative store and require registered path, NAR hash, NAR size, references, deriver, and absent content address to match the normalized declaration.
7. Delete staging state on success or failure.

No authoritative-store mutation occurs before steps 1–4 succeed.

## Nix guarantees retained at the boundary

Pinned Nix `LocalStore::addToStore` restores the NAR through its parser while independently hashing the received stream, compares the resulting hash and byte count with `ValidPathInfo`, validates a declared content address when present, canonicalizes store metadata, and registers validity only after those checks. Registration uses a SQLite transaction for path metadata and references.

Nix remains the final defense against staged-file corruption between Telchar validation and promotion: a changed staged NAR fails the explicit NAR hash or size comparison supplied in `ValidPathInfo`.

## Failure semantics

A failed `Store::addToStore` call is an import failure. Telchar must query path validity after failure and must not report success merely because the helper exited or emitted output. Tests must prove rejected content or metadata leaves no valid authoritative registration. Filesystem residue after a failed Nix restore is treated as a fixture/store defect and must be checked separately from database validity.

## Packaging

The helper is built reproducibly from the same flake-pinned Nix package used as the compatibility oracle. It links through `nix-store.pc` and is packaged beside `telchar`. Native and container deployments continue to run only:

- `telchar daemon`
- `telchar serve-stdio`

The daemon invokes the private helper with a fixed configured store endpoint. Operators do not invoke or configure it independently.

## Rejected alternatives

### Legacy `nix-store --import`

Rejected as the production validation boundary. Its export envelope does not carry an independent expected NAR hash; import computes the hash from received bytes and associates it with the path named by the same envelope.

### `nix store add`

Rejected for this operation. It computes a content-addressed destination path rather than registering the client-declared classic input-addressed path and metadata.

### Stable Nix C API

Not currently sufficient. The pinned C API exposes validity queries and derivation insertion but not `ValidPathInfo` plus raw-NAR registration.

### Handwritten worker client in the first adapter

Deferred. `AddToStoreNar` is an evidenced production protocol operation, but duplicating Nix connection negotiation, stderr activity handling, version gates, and framed-stream behavior adds unnecessary compatibility risk before the store boundary itself is proven.
