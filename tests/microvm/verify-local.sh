#!/usr/bin/env bash
# Rebuild fixture executables and verify local boundaries with disposable stores.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PYTHONDONTWRITEBYTECODE=1
python3 tests/microvm/test_runtime.py
python3 tests/microvm/test_bootstrap.py
cargo build --locked -p telchar-nomad-worker --examples
cargo test --locked -p telchar-nomad-worker --example microvm-receiver -- --test-threads=1
python3 tests/microvm/test_receiver.py
python3 tests/microvm/local-exchange.py
