#!/usr/bin/env bash
# Run workspace packages with their required store isolation boundaries.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -m unittest discover -s tests/microvm -p test_store_capacity.py -v
unshare --user --map-auto --map-root-user --mount --pid --fork --mount-proc \
    python3 -m unittest discover -s tests/microvm -p test_retained_store.py -v
bash tests/canonical-store.sh cargo test "$@" --workspace --exclude telchar-nomad-worker -- --test-threads=1
cargo test "$@" -p telchar-nomad-worker -- --test-threads=1
