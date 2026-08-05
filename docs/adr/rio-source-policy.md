# Rio source policy

**Status:** Accepted

## Context

The archived rio-build reference at [`59e832144d67c1b1973272ef394ffc6ef2629f4b`](../rio-build-reference.md) has conflicting licensing signals:

- Its root [`LICENSE`](https://github.com/lovesegfault/rio-build/blob/59e832144d67c1b1973272ef394ffc6ef2629f4b/LICENSE) declares the BSD 3-Clause License.
- Its workspace [`Cargo.toml`](https://github.com/lovesegfault/rio-build/blob/59e832144d67c1b1973272ef394ffc6ef2629f4b/Cargo.toml#L27-L32) declares `MIT OR Apache-2.0`, including the `rio-nix` workspace member.

The discrepancy prevents this packet from treating either signal as sufficient authorization for source reuse.

## Decision

Initial `nix-worker-protocol` code must not copy, translate, or mechanically adapt Rio source or tests.

Reference evidence is limited to architecture observations and test-category research. It must identify the immutable revision and cannot establish wire behavior.

Implementation evidence is captured stock-Nix traffic plus primary Nix source, serializers, or documentation. Every protocol behavior must cite this evidence rather than Rio implementation details.

Any future source import requires a separate explicit import decision. That decision must resolve applicable licensing and copyright scope, identify exact imported paths and revisions, record attribution and notice obligations, obtain project approval, and add license-compliance verification before import.

## Consequences

The implementation may independently recreate a behavior supported by primary Nix evidence, but it cannot use Rio text, structure, tests, or transformations as its starting material. This avoids ambiguous copying while keeping the archived repository available for bounded reference research.
