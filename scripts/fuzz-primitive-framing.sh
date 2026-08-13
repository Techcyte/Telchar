#!/bin/sh
# Runs the worker-protocol primitive framing fuzz target in the Nix development shell.
set -eu

cd crates/nix-worker-protocol/fuzz
RUSTC_BOOTSTRAP=1 nix shell nixpkgs#cargo-fuzz nixpkgs#clang --command \
	cargo fuzz run primitive_framing -- -runs="${FUZZ_RUNS:-1000}" -seed=1413829460
