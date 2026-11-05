"""Summarize successful connected-reuse benchmark JSON records (milliseconds/bytes)."""
import json
import statistics
import sys
from pathlib import Path


def summarize(text):
    records = [json.loads(line.split("MICROVM_BENCH ", 1)[1])
               for line in text.splitlines() if "MICROVM_BENCH " in line]
    groups = {mode: [record for record in records if record["mode"] == mode]
              for mode in ("cold", "warm")}
    if not groups["cold"] or len(groups["cold"]) != len(groups["warm"]):
        raise ValueError("expected equal nonempty cold and warm samples")
    print("Small sample: median / observed max; not a p95/p99 estimate.")
    for mode, group in groups.items():
        print(f"{mode}: n={len(group)}")
        for key in ("job_reset_ms", "hook_start_ms", "connect_auth_ms",
                    "input_transfer_ms", "after_input_through_result_ms", "reset_ms",
                    "input_nar_bytes", "output_nar_bytes"):
            values = [record[key] for record in group]
            print(f"  {key}: {statistics.median(values):.3f} / {max(values):.3f}")
    cold = statistics.median(record["job_reset_ms"] for record in groups["cold"])
    warm = statistics.median(record["job_reset_ms"] for record in groups["warm"])
    print(f"median cold/warm ratio: {cold / warm:.3f}x")


if __name__ == "__main__":
    summarize(Path(sys.argv[1]).read_text())
