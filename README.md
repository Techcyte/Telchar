# Telchar

A lightweight Nix build distributor.

## Status

Early implementation. Telchar is not ready for production use.

## Trust boundary

Initial deployments are Linux-first and single-active: one Telchar daemon owns scheduling, durable state, gateway-store coordination, and backend reconciliation. Authenticated clients share one mutually trusted Nix store domain. This is not hostile multi-tenant isolation or per-path authorization.

Read the [design brief](telchar-design.md) and [implementation plan](TELCHAR_IMPLEMENTATION_PLAN.md) before changing the system.

## Development

Requirements: Nix with `nix-command` and `flakes` experimental features enabled.

From repository root, inspect pinned inputs and enter the reproducible development shell:

```sh
nix flake metadata
nix develop
```

Build the workspace:

```sh
nix develop -c cargo build --workspace --locked
```

Run the individual checks:

```sh
nix develop -c cargo fmt --check
nix develop -c cargo clippy --all-targets --all-features -- -D warnings
nix develop -c cargo test --lib --locked
```

Run every repository check:

```sh
nix flake check
```
