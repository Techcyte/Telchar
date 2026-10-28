#!/usr/bin/env bash
# Run workspace packages with their required store isolation boundaries.
set -euo pipefail
cd "$(dirname "$0")/.."
bash tests/canonical-store.sh cargo test "$@" --workspace --exclude telchar-nomad-worker -- --test-threads=1
cargo test "$@" -p telchar-nomad-worker -- --test-threads=1
