#!/bin/sh
set -eu

allowlist='docs/worker-operation-allowlist.md'
traces='docs/compatibility-traces'

[ -f "$allowlist" ] || {
	printf 'missing worker-operation allowlist: %s\n' "$allowlist" >&2
	exit 1
}

python3 - "$allowlist" "$traces" <<'PY'
import json
import sys
from pathlib import Path

allowlist, traces = map(Path, sys.argv[1:])
text = allowlist.read_text()
required_text = [
    '## Required operations',
    '`SetOptions` (`19`)',
    '`IsValidPath` (`1`)',
    '`AddToStore` (`7`)',
    '`QueryMissing` (`40`)',
    '`QueryPathInfo` (`26`)',
    '`BuildPathsWithResults` (`46`)',
    '## Optional operations',
    '`AddTempRoot` (`11`)',
    '## Recognized and rejected operations',
    '## Unknown operations',
]
for value in required_text:
    if value not in text:
        raise SystemExit(f'missing allowlist classification: {value}')

classified = {1, 7, 11, 19, 26, 40, 46}
for fixture in ('trusted-classic-build-v1.json', 'untrusted-classic-build-v1.json'):
    path = traces / fixture
    if not path.is_file():
        raise SystemExit(f'missing trace artifact: {path}')
    trace = json.loads(path.read_text())
    if set(trace) != {'fixture', 'client_protocol', 'peer_protocol', 'trusted', 'operations', 'output_sha256'}:
        raise SystemExit(f'unsafe trace artifact fields: {path}')
    if not isinstance(trace['fixture'], str) or len(trace['fixture']) > 64:
        raise SystemExit(f'invalid fixture identifier: {path}')
    if not isinstance(trace['trusted'], bool):
        raise SystemExit(f'invalid trust result: {path}')
    if not isinstance(trace['operations'], list) or not all(isinstance(code, int) for code in trace['operations']):
        raise SystemExit(f'invalid operation list: {path}')
    unclassified = sorted(set(trace['operations']) - classified)
    if unclassified:
        raise SystemExit(f'unclassified operations in {path}: {unclassified}')

print('worker operation allowlist check passed: zero unclassified operations')
PY
