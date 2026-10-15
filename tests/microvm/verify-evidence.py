#!/usr/bin/env python3
"""Check preserved AWS execution evidence; never provision or contact AWS."""
import base64
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
def load(path):
    return json.loads(path.read_text())
ledger = load(root / "resources.json")
assert ledger["cleanup"] == "complete"
cleanup = load(root / "cleanup-verification.json")
assert len(ledger["microvmIds"]) == 7
assert all(value in ("absent", "terminated") for value in cleanup.values())
for key in ("instance", "security_group", "key_pair", "image", "bucket", "role", "log_group"):
    assert key in cleanup
for volume in ledger["volumeIds"]:
    assert cleanup["volume:" + volume] == "absent"
events = load(root / "cloudwatch-final.json")
payload = b"closure transport payload\n" * 40000
for label in ("success-2", "measurement-success", "timed-success"):
    directory = root / label
    result = load(directory / "result.json")
    vm = result["microvmId"]
    assert result["outcome"] == "succeeded"
    assert result["source_bytes"] == len(payload)
    assert cleanup[vm] == "terminated"
    state = load(root / (vm + "-state.json"))
    assert state["microvmId"] == vm and state["state"] == "TERMINATED"
    messages = "\n".join(event["message"] for event in events if vm in event.get("logStreamName", ""))
    assert "microvm outbound build transfer succeeded" in messages
    assert '"outcome": "succeeded"' in messages
    assert not any(word in messages.lower() for word in ("error:", "warning:", '"outcome": "failed"'))
    assert not (directory / "receiver.stderr").read_bytes()
    assert (directory / "receiver.stdout").read_text().splitlines() == ["execution authentication accepted"]
    info = load(directory / "path-info.json")
    assert len(info) == 1
    output_path, metadata = next(iter(info.items()))
    nar = (directory / "output.nar").read_bytes()
    assert len(nar) == metadata["narSize"]
    assert "sha256-" + base64.b64encode(hashlib.sha256(nar).digest()).decode() == metadata["narHash"]
    output = (directory / "output").read_bytes()
    assert len(output) == result["output_bytes"]
    with tempfile.TemporaryDirectory(prefix="telchar-evidence-") as temporary:
        destination = Path(temporary) / "output"
        restored = subprocess.run(["nix-store", "--restore", str(destination)], input=nar, capture_output=True)
        assert restored.returncode == 0 and not restored.stderr, restored.stderr.decode()
        assert destination.read_bytes() == output
    if label == "success-2":
        assert output == payload
    else:
        report, returned = output.split(b"PAYLOAD\n", 1)
        assert returned == payload
        assert report == (directory / "worker-resources.txt").read_bytes()
        assert report.startswith(b"REPORT\n") and b"MemTotal:" in report and b"/dev/vdc" in report
        manifest = load(directory / "prepared.json")["manifest"]
        assert manifest["build"]["system"] == "aarch64-linux"
        assert manifest["outputs"] == [output_path]
    print(f"AWS preserved {label}: validated NAR {len(nar)} bytes; payload matches; lifecycle succeeded; terminated")
timed = root / "timed-success"
assert list(load(timed / "timed-output-before.json").values()) == [None]
phases = [json.loads(line) for line in (timed / "timed-phases.jsonl").read_text().splitlines()]
assert [event["phase"] for event in phases] == ["manifest", "input_request", "inputs_sent", "build_started", "output_metadata", "output_registered", "build_result"]
assert all(b["seconds"] >= a["seconds"] for a, b in zip(phases, phases[1:], strict=False))
assert phases[2]["bytes"] == 1040112 and phases[-1]["bytes"] == 1040376
corrupt = "microvm-7d7b3785-4bc5-3fa3-a2b4-fa385d489ffb"
messages = "\n".join(event["message"] for event in events if corrupt in event.get("logStreamName", ""))
assert "worker input import failed" in messages and "AddToStoreNar was rejected" in messages
assert '"outcome": "failed"' in messages and cleanup[corrupt] == "terminated"
negative = load(root / "negative.json")
assert negative["wrong_execution"] == "HTTP/1.1 403 Forbidden"
assert negative["correct_binding"] == "HTTP/1.1 101 Switching Protocols"
assert "UnexpectedEof" in (root / "negative.stderr").read_text()
inspection = load(root / "container-inspect.json")[0]
assert not inspection["Mounts"], "receiver must not mount host stores"
print("AWS preserved negatives, phase ordering, output absence before run, isolated receiver, and cleanup evidence: PASS")
print("Scope: archived AWS observations, not a fresh AWS execution or live cleanup query.")
