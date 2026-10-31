#!/usr/bin/env bash
# Runs the authoritative package, workspace, and selected NixOS release verification.
set -euo pipefail

cd "$(dirname "$0")/.."

export NIXPKGS_ALLOW_UNFREE=1

nix build -L --no-link \
  .#checks.x86_64-linux.format \
  .#checks.x86_64-linux.workspace-check \
  .#checks.x86_64-linux.lint \
  .#checks.x86_64-linux.workspace-tests
nix build -L --no-link .#checks.x86_64-linux.release-tooling
# Live advisory freshness and exception expiry must not use a cached result.
nix develop -c scripts/check-advisory-exceptions.py
nix develop -c cargo deny check advisories licenses sources

nix build --no-link .#telchar .#telchar-nomad-worker
nix build --no-link .#checks.x86_64-linux.oci-images
scripts/check-oci-images.sh
nix build --no-link .#checks.aarch64-linux.oci-images
IMAGE_SYSTEM=aarch64-linux scripts/check-oci-images.sh
nix build --no-link .#checks.x86_64-linux.nixos-oci-runtime
nix build --no-link .#checks.x86_64-linux.nixos-module
nix build --no-link .#checks.x86_64-linux.nixos-microvm-service
nix build --no-link .#checks.x86_64-linux.nixos-remote-build-contract
nix build --no-link .#checks.x86_64-linux.nixos-lix-local
nix build --no-link .#checks.x86_64-linux.nixos-fixed-output-local
nix build --no-link .#checks.x86_64-linux.nixos-oci-gateway
nix build --no-link .#checks.x86_64-linux.nixos-static-ssh-gateway
nix build --impure --no-link .#checks.x86_64-linux.nixos-nomad-gateway
