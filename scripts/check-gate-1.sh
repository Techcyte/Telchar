#!/bin/sh
set -eu

expected_nix_version='nix (Nix) 2.34.7'
actual_nix_version="$(nix develop -c nix --version)"
[ "$actual_nix_version" = "$expected_nix_version" ]

nix develop -c cargo fmt --check
nix develop -c cargo clippy --all-targets --all-features --locked -- -D warnings
nix develop -c cargo test --workspace --locked
nix develop -c cargo test --test operation_dispatch --locked partial_set_options_times_out_after_operation_and_cleans_up
nix develop -c cargo test --test operation_dispatch --locked partial_set_options_progress_resets_deadline
nix develop -c cargo test -p nix-worker-protocol --lib --locked rejects_oversized_worker_error_before_writing
FUZZ_RUNS="${FUZZ_RUNS:-1000}" sh scripts/fuzz-primitive-framing.sh
sh scripts/check-protocol-evidence-inventory.sh
sh scripts/check-protocol-fixture-flow-inventory.sh
sh scripts/check-worker-operation-allowlist.sh
sh scripts/check-protocol-session-limits.sh
sh scripts/check-rio-test-category-inventory.sh
sh scripts/check-telemetry-contract.sh
printf 'Gate 1 stdio protocol proof passed\n'
