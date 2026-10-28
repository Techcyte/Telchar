#!/usr/bin/env python3
"""Validate preserved substantial AWS workload evidence without AWS access."""
import base64
import hashlib
import json
import sys
from pathlib import Path


def verify_archive(path, metadata):
    assert path.stat().st_size == metadata["narSize"], str(path)
    with path.open("rb") as archive:
        digest = hashlib.file_digest(archive, "sha256").digest()
    assert "sha256-" + base64.b64encode(digest).decode() == metadata["narHash"], str(path)


def main(root):
    def load(name):
        return json.loads((root / name).read_text())

    ledger = load("resources.json")
    assert ledger["name"] == "telchar-microvm-workloads-1792088528"
    assert ledger["cleanup"] == "complete"
    cleanup = load("cleanup-verification.json")
    for key in ("instance", "security_group", "key_pair", "image", "image_version:1.0", "image_version:2.0", "bucket", "role", "log_group"):
        assert cleanup[key] == ("terminated" if key == "instance" else "absent")
    for volume in ledger["volumeIds"]:
        assert cleanup["volume:" + volume] == "absent"
    assert len(ledger["microvmIds"]) == 4
    for vm in ledger["microvmIds"]:
        assert cleanup[vm] == "terminated"
    result = load("compile-4/result.json")
    assert result["outcome"] == "registered" and result["elapsed_seconds"] < 600
    assert result["microvmId"] in ledger["microvmIds"]
    assert not (root / "compile-4/receiver.stderr").read_bytes()
    assert (root / "compile-4/receiver.stdout").read_text().splitlines() == ["execution authentication accepted"]
    absence = load("before-output-absence.json")
    assert absence["all_absent"] is True
    assert sorted(absence["outputs"].values()) == sorted(result["outputs"])
    total = 0
    for index, path in enumerate(result["outputs"]):
        metadata = load(f"compile-4/output-{index}-path-info.json")
        assert list(metadata) == [path]
        verify_archive(root / f"compile-4/output-{index}.nar", metadata[path])
        total += metadata[path]["narSize"]
    assert total == 574136960
    assert (root / "compile-4/outputs.export").stat().st_size == 574138408
    phases = [json.loads(line) for line in (root / "compile-4/receiver-timing.jsonl").read_text().splitlines()]
    assert [p["phase"] for p in phases] == ["manifest", "input_request", "inputs_sent", "build_started", "output_metadata", "output_registered", "output_metadata", "output_registered", "build_result"]
    assert all(b["seconds"] >= a["seconds"] for a, b in zip(phases, phases[1:], strict=False))
    assert phases[2]["bytes"] == 373489368
    assert phases[5]["bytes"] == 57537296
    assert phases[-2]["bytes"] == phases[-1]["bytes"] == total
    assert not load("package-current-container-inspect.json")[0]["Mounts"]
    assert not load("receiver-after-inspect.json")[0]["Mounts"]
    print(f"Archived AWS substantial native ARM build: registered {total} NAR bytes in {result['elapsed_seconds']:.3f}s; hashes and cleanup evidence PASS")
    print("Scope: archived observations, not fresh AWS execution or live cleanup queries. Worker peak memory/disk and actual billing unmeasured.")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
