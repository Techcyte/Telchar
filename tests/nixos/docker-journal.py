import json
import re


def check_docker_journal(machine, *, signature_errors=0, channel_errors=0, after=None):
    command = "journalctl -b -u docker.service --no-pager -o json"
    if after is not None:
        command += " --after-cursor='" + after + "'"
    records = [json.loads(line) for line in machine.succeed(command).splitlines()]
    signatures = 0
    channels = 0
    for record in records:
        message = record.get("MESSAGE", "")
        assert "Failed to find nft tool" not in message, message
        if 'msg="Deleting nftables ' in message and 'error=' in message:
            assert "No such file or directory" in message, message
        if 'level=error' not in message and 'level=warning' not in message:
            continue
        if 'level=warning' in message:
            assert 'msg="Configuration migrated from version 2, use `containerd config migrate` to avoid migration"' in message, message
        elif re.search(r'msg="failed to validate image signature" error="resolving signature chain for image sha256:[0-9a-f]{64}: expected image index descriptor, got application/vnd.docker.distribution.manifest.v2\+json"$', message):
            signatures += 1
        elif 'msg="failed sending message on channel" error="write unix /var/run/docker/containerd/containerd.sock.ttrpc->@: write: broken pipe"' in message:
            channels += 1
        else:
            raise AssertionError(message)
    assert signatures <= signature_errors, (signatures, signature_errors)
    assert channels <= channel_errors, (channels, channel_errors)
    return channels
