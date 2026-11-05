# Connected cold/warm reuse measurement

The normal `microvm_reuse::sequential_jobs_reuse_one_supervised_worker` test stays small.
The ignored `microvm_reuse::cold_warm_connected_benchmark` uses **the same
`connected_jobs` implementation**, including production ExecutionState, distinct Rust
worker processes, TLS/WebSocket transport, retained input imports, checkpointed
metadata, private overlay/daemon, gateway output validation/registration, and durable
assignment/reset checks. Every build hashes the entire incompressible shared input
and returns that hash with a distinct job suffix; the gateway verifies the content.
Each build also checks that the other jobs' outputs are absent. Warm jobs keep the
same helper PID, retained root, owner generation and durable provider identity, and
must omit the shared input from their requests. No isolation or scan is bypassed.

## Run

Requires the flake development environment, working unprivileged user/mount/PID
namespaces, overlayfs and subordinate UID/GID mappings, as for the connected test.
Run as a nonroot user; PostgreSQL and all stores are private fixture state.

```sh
mkdir -p "$HOME/.local/state/microvm-connected-reuse"
set -o pipefail
CARGO_BUILD_JOBS=2 MICROVM_BENCH_SAMPLES=3 MICROVM_BENCH_INPUT_MIB=32 \
  nix develop -c cargo test --locked -p telchar-nomad-worker \
    --test microvm_build microvm_reuse::cold_warm_connected_benchmark \
    -- --exact --ignored --nocapture --test-threads=1 \
  2>&1 | tee "$HOME/.local/state/microvm-connected-reuse/benchmark.log"
# With Bash, inspect both statuses immediately: statuses=("${PIPESTATUS[@]}")
python3 tests/microvm/summarize-reuse.py \
  "$HOME/.local/state/microvm-connected-reuse/benchmark.log"
```

Defaults: three measured jobs per mode, 32 MiB shared random input. Bounds:
1–10 samples and 1–64 MiB (bounded for the unchanged 30-second job/session deadline). The input is generated once per independent fixture
from `/dev/urandom`; all jobs in a warm fixture share that exact closure. There
are no timing assertions. Each cold sample has a new fixture/database/helper/root
and epoch 1. The warm fixture performs one excluded cold seed and then the requested
number of measured epochs. Derivations/output identities differ across epochs.
The normal Rust test summary must pass before treating the measurements as valid.

## Measurement boundaries

Each `MICROVM_BENCH` JSON record contains milliseconds and NAR payload bytes:

- `job_reset_ms`: before helper spawn (cold/seed) or next job claim (warm), through
  verified production teardown and durable reset. Includes cold helper bootstrap
  and copying the builder closure. Excludes fixture PostgreSQL startup, schema
  migration, gateway startup, input generation and request construction (common
  test setup), and excludes final helper termination.
- `hook_start_ms`: synchronous provider-hook command/acceptance, including waiting
  for initial helper setup on a new worker.
- `connect_auth_ms`: wait for worker TLS connection and authenticated assignment.
- `input_transfer_ms`: input request handling through last NAR forwarded. Measures
  gateway export/validation/relay/backpressure, not final worker import completion.
- `after_input_through_result_ms`: last input forwarded through worker build result;
  includes remaining import, capacity checks, checkpoint, overlay/daemon preparation,
  actual build/hash, output validation/receipt, and protocol completion. These are
  intentionally not mislabeled as separately measured daemon or scan times.
- `reset_ms`: after build-result relay through gateway outcome checks, durable
  completion, joining ExecutionState, clean private-state checks and durable reset.
- `input_nar_bytes` / `output_nar_bytes`: NAR payload only, excludes framing/TLS and
  authentication metadata. Cold includes missing dependency closures, warm should
  be zero for this unchanged shared closure.

These phase intervals are coarse and not a complete additive partition.
The summary reports median and observed maximum, not a statistically meaningful
p95/p99 from a tiny sample. Cold runs precede warm runs; host filesystem/page caches
are not flushed. This compares fresh **worker state**, not cold host caches.
Provider launch/hook delivery and TLS termination are simulated local seams; AWS
boot, network latency, snapshot restore and ARM image behavior are not measured.
Do not infer a production bottleneck (such as capacity scans or daemon startup)
from an aggregate phase alone. Only optimize a demonstrated dominant cost while
preserving all production isolation and reset boundaries.
