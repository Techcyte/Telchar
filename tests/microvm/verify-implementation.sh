#!/usr/bin/env bash
# Local checkpoint for implemented boundaries, not production backend qualification.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PYTHONDONTWRITEBYTECODE=1 CARGO_BUILD_JOBS=1
python3 - <<'PY'
from pathlib import Path

membership = Path('/proc/self/cgroup').read_text().strip()
if not membership.startswith('0::'):
    raise SystemExit('unified cgroup containment required')
root = Path('/sys/fs/cgroup') / membership.removeprefix('0::').lstrip('/')
for name, ceiling in [('memory.max', 8 * 1024**3), ('memory.swap.max', 0), ('pids.max', 256)]:
    value = (root / name).read_text().strip()
    if value == 'max' or int(value) > ceiling:
        raise SystemExit(f'unsafe {name}: {value}')
    print(f'{name}={value}', flush=True)
quota, period = (root / 'cpu.max').read_text().split()
if quota == 'max' or int(quota) > 2 * int(period):
    raise SystemExit('CPU quota must not exceed 200%')
print(f'cpu.max={quota} {period}', flush=True)
PY
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked -p telchar \
  --test microvm_launches --test microvm_payload --test microvm_pending \
  --test microvm_provider --test microvm_provider_cleanup --test microvm_rendezvous \
  --test microvm_config --test microvm_handoff --test microvm_identity \
  --test microvm_listener --test microvm_outputs --test microvm_service \
  --test persistence_migrations -- --test-threads=1
cargo build --locked -p telchar --bin telchar
export TELCHAR_TEST_DAEMON_BIN="$PWD/target/debug/telchar"
cargo test --locked -p telchar-nomad-worker \
  --test microvm_admission --test microvm_connector --test microvm_deadline \
  --test microvm_process --test microvm_session --test microvm_build \
  --test worker -- --test-threads=1
for suite in hook execution runtime bootstrap nix_execution; do
  unshare --user --map-root-user --mount --propagation private python3 "tests/microvm/test_${suite}.py"
done
printf 'MICROVM_IMPLEMENTATION_CHECKPOINT=passed\n'
