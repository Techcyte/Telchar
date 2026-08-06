#!/bin/sh
set -eu

require_output() {
	command=$1
	expected=$2

	output=$(sh -c "$command")
	printf '%s\n' "$output"
	printf '%s\n' "$output" | grep -F -- "$expected" >/dev/null || {
		printf 'missing expected output: %s\n' "$expected" >&2
		exit 1
	}
}

nix_reference=$(nix build --no-link --print-out-paths '.#nix-reference^out')
expected_nix_version=$(nix eval --raw .#nix-reference.version)
require_output "$nix_reference/bin/nix --version" "nix (Nix) $expected_nix_version"
require_output 'nix develop -c rustc --version' 'rustc 1.95.0'
require_output 'nix develop -c cargo --version' 'cargo 1.95.0'

grep -F -- '- [x] T021 Verify Gate 0 from clean checkout' TELCHAR_IMPLEMENTATION_PLAN.md >/dev/null

nix flake check
nix develop -c cargo test -p telchar exports_otlp_signals_before_application_work --bin telchar --locked

sh scripts/check-deployment-assumptions.sh
sh scripts/check-telemetry-contract.sh
sh scripts/check-telemetry-dependencies.sh
sh scripts/check-telemetry-failure-bounds.sh
sh scripts/check-nix-compatibility-matrix.sh
sh scripts/check-rio-build-reference.sh
sh scripts/check-rio-source-policy.sh
sh scripts/check-protocol-boundary.sh
sh scripts/check-protocol-fixture-flow-inventory.sh
sh scripts/check-worker-operation-allowlist.sh
sh scripts/check-protocol-evidence-inventory.sh

printf 'Gate 0 verification passed\n'
