#!/bin/sh
set -eu

nix_reference="$(nix build --no-link --print-out-paths '.#nix-reference^out')"
expected_nix_version="$(nix eval --raw .#nix-reference.version)"
actual_nix_version="$($nix_reference/bin/nix --version)"
[ "$actual_nix_version" = "nix (Nix) $expected_nix_version" ]
export TELCHAR_NIX_BIN="$nix_reference/bin/nix"

nix develop -c cargo fmt --check
nix develop -c cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
nix develop -c env TELCHAR_NIX_BIN="$TELCHAR_NIX_BIN" cargo test -p telchar --test openssh_ingress --locked -- --nocapture
nix build --no-link '.#checks.x86_64-linux.nixos-gate-2'
sh scripts/check-openssh-fixture.sh
sh scripts/check-supported-identity-path.sh
sh scripts/check-requester-normalization.sh
sh scripts/check-telemetry-contract.sh
printf 'Gate 2 restricted OpenSSH ingress passed\n'
