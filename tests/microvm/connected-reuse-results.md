# Local connected reuse result (2026-11-05)

Command and measurement definitions: [connected-reuse.md](connected-reuse.md).
Default workload: 32 MiB kernel-random shared input, three fresh-worker cold jobs,
one excluded seed followed by three persistent-worker warm jobs. Each job hashes
all input bytes and returns a different verified output identity/content.

Recorded invocation:

```sh
CARGO_BUILD_JOBS=2 nix develop -c cargo test --locked \
  -p telchar-nomad-worker --test microvm_build \
  microvm_reuse::cold_warm_connected_benchmark \
  -- --exact --ignored --nocapture --test-threads=1
```

All jobs, production reset checks, omitted warm-input checks, gateway registrations
and receipts passed. Benchmark test runtime: 57.11 seconds (excludes compilation).
Logs remain outside the repository at
`~/.local/state/microvm-connected-reuse/benchmark-32m-3-v3.log`.

| Measurement | Cold median / observed max | Warm median / observed max |
| --- | ---: | ---: |
| Job through clean durable reset | 12123.102 / 12140.978 ms | 592.978 / 619.157 ms |
| Hook acceptance (includes cold helper setup) | 519.962 / 601.494 ms | 1.024 / 1.030 ms |
| Connection/authentication | 196.308 / 245.924 ms | 205.395 / 244.712 ms |
| Input transfer | 10897.023 / 10926.061 ms | 0.027 / 0.028 ms |
| After input through result | 291.432 / 313.690 ms | 260.325 / 261.302 ms |
| Reset/verification after result | 80.476 / 86.440 ms | 69.770 / 70.601 ms |
| Input NAR payload | 46,884,656 bytes each | 0 bytes each |
| Output NAR payload | 192 bytes each | 192 bytes each |

Median local cold/warm ratio: **20.444x**. Shared-input transfer is the dominant
measured cold cost (~90% of job/reset wall time), and reuse removes it. The coarse
remaining phases do not identify capacity scans or daemon startup as dominant.
No additional production optimization is justified by this evidence; none was made.
Further transfer profiling could distinguish export/framing/TLS/backpressure costs,
but these measurements alone do not authorize a transport or isolation redesign.

Three samples cannot establish p95/p99 or production performance. The provider and
TLS-termination relay are local simulations, host caches were not flushed, and cold
runs precede warm runs. Fresh-worker timings include helper bootstrap/copying its
builder closure, not AWS launch/snapshot/ARM execution. PostgreSQL/schema/gateway
fixture setup and random-input generation are outside the per-job timing.
