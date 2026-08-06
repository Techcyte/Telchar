#!/bin/sh
set -eu

nix develop -c cargo fmt --check
nix develop -c cargo clippy --all-targets --all-features --locked -- -D warnings
nix develop -c cargo test --workspace --locked
FUZZ_RUNS="${FUZZ_RUNS:-1000}" sh scripts/fuzz-primitive-framing.sh
sh scripts/check-protocol-evidence-inventory.sh
sh scripts/check-protocol-fixture-flow-inventory.sh
sh scripts/check-worker-operation-allowlist.sh
sh scripts/check-protocol-session-limits.sh
sh scripts/check-rio-test-category-inventory.sh
sh scripts/check-telemetry-contract.sh
printf 'Gate 1 stdio protocol proof passed\n'
