import json
import re


def check_nomad_journal(machine):
    records = [json.loads(line) for line in machine.succeed(
        "journalctl -b -u nomad.service --no-pager -o json"
    ).splitlines()]
    expected = [
        r'\[WARN\]  nomad\.raft: heartbeat timeout reached, starting election:',
        r'\[WARN\]  nomad\.client: node registration without introduction token: enforcement_level=warn ',
        r'\[WARN\]  agent\.plugin_loader: skipping external plugins since plugin_dir doesn\'t exist: plugin_dir=/nix/store/[a-z0-9]+-nomad-plugins/bin',
        r'\[WARN\]  client\.fingerprint_mgr: failed to detect bridge kernel module, bridge network mode disabled:',
        r'\[WARN\]  client\.fingerprint_mgr\.consul: failed to acquire consul self endpoint: cluster=default error="Get \\"http://localhost:8500/v1/agent/self\\": dial tcp \[::1\]:8500: connect: connection refused"',
        r'\[WARN\]  client\.fingerprint_mgr\.cni_plugins: failed to read CNI plugins directory: cni_path=(/opt/cni/bin|/usr/libexec/cni) error="open (/opt/cni/bin|/usr/libexec/cni): no such file or directory"',
        r'\[ERROR\] client\.driver_mgr\.docker: failed to list pause containers for recovery: driver=docker error="Cannot connect to the Docker daemon at unix:///var/run/docker.sock\. Is the docker daemon running\?"',
    ]
    patterns = [re.compile(pattern) for pattern in expected]
    counts = [0] * len(patterns)
    for record in records:
        message = record.get("MESSAGE", "")
        if "[WARN]" not in message and "[ERROR]" not in message:
            continue
        matches = [index for index, pattern in enumerate(patterns) if pattern.search(message)]
        assert len(matches) == 1, message
        counts[matches[0]] += 1
    assert all(count <= 2 for count in counts), counts
