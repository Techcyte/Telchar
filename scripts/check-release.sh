#!/usr/bin/env bash
# Runs the authoritative package, workspace, and selected NixOS release verification.
set -euo pipefail

cd "$(dirname "$0")/.."

export NIXPKGS_ALLOW_UNFREE=1

nix develop -c cargo fmt --all -- --check
nix develop -c scripts/test-prepare-release.py
nix develop -c cargo test --locked --workspace -- --test-threads=1
nix develop -c cargo check --locked --workspace
nix develop -c cargo clippy --locked --workspace --all-targets -- -D warnings
nix develop -c scripts/check-advisory-exceptions.py
nix develop -c cargo deny check advisories licenses sources

nix build --no-link .#telchar .#telchar-nomad-worker
nix build --no-link .#checks.x86_64-linux.oci-images
scripts/check-oci-images.sh
nix build --no-link .#checks.x86_64-linux.nixos-oci-runtime
nix build --no-link .#checks.x86_64-linux.nixos-module
nix build --no-link .#checks.x86_64-linux.nixos-remote-build-contract
nix build --no-link .#checks.x86_64-linux.nixos-lix-local
nix build --no-link .#checks.x86_64-linux.nixos-fixed-output-local
nix build --no-link .#checks.x86_64-linux.nixos-oci-gateway
nix build --no-link .#checks.x86_64-linux.nixos-static-ssh-gateway
nix build --impure --no-link .#checks.x86_64-linux.nixos-nomad-gateway
nix develop -c cargo test --locked -p telchar --test shared_build_recovery
