#!/usr/bin/env bash
# Local checkpoint for implemented boundaries, not production backend qualification.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PYTHONDONTWRITEBYTECODE=1
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked -p telchar \
  --test microvm_launches --test microvm_payload --test microvm_pending \
  --test microvm_provider --test microvm_provider_cleanup --test microvm_rendezvous \
  --test persistence_migrations -- --test-threads=1
cargo test --locked -p telchar-nomad-worker \
  --test microvm_admission --test microvm_connector --test microvm_deadline \
  --test microvm_process --test microvm_session --test worker -- --test-threads=1
for suite in hook execution runtime bootstrap nix_execution; do
  python3 "tests/microvm/test_${suite}.py"
done
printf 'MICROVM_IMPLEMENTATION_CHECKPOINT=passed\n'
