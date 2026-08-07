#!/bin/sh
set -eu

manifest='docs/classic-build-operation-coverage.json'
allowlist='docs/worker-operation-allowlist.md'
inventory='docs/protocol-fixture-flow-inventory.md'
traces='docs/compatibility-traces'

for path in "$manifest" "$allowlist" "$inventory"; do
	[ -f "$path" ] || {
		printf 'missing classic operation coverage input: %s\n' "$path" >&2
		exit 1
	}
done

python3 - "$manifest" "$allowlist" "$inventory" "$traces" <<'PY'
import json
import sys
from pathlib import Path

manifest_path, allowlist_path, inventory_path, traces_path = map(Path, sys.argv[1:])
manifest = json.loads(manifest_path.read_text())
allowlist = allowlist_path.read_text()
inventory = inventory_path.read_text()

if manifest.get('schema') != 'telchar.classic-build-operation-coverage/v1':
    raise SystemExit('unexpected operation coverage schema')
scope = manifest.get('scope')
if not isinstance(scope, dict) or scope.get('production_only') is not True:
    raise SystemExit('manifest must require production-only coverage')
if scope.get('observer_relay_is_coverage') is not False:
    raise SystemExit('observer relay must not satisfy production coverage')

operations = manifest.get('operations')
if not isinstance(operations, list) or not operations:
    raise SystemExit('manifest has no operations')

by_code = {}
for operation in operations:
    required = (
        'name', 'code', 'fixture_role', 'admission', 'production_packet',
        'decoder', 'dispatcher', 'store_behavior', 'response_framing',
        'compatibility_test', 'primary_sources', 'inventory_boundary'
    )
    missing = [field for field in required if not operation.get(field) or (field == 'code' and not isinstance(operation.get(field), int)) or (field != 'code' and not isinstance(operation.get(field), (str, list)))]
    if missing:
        raise SystemExit(f"operation {operation.get('name', '<unnamed>')} missing fields: {', '.join(missing)}")
    code = operation['code']
    if not isinstance(code, int) or code < 0 or code in by_code:
        raise SystemExit(f'invalid or duplicate operation code: {code!r}')
    if operation['admission'] != 'required':
        raise SystemExit(f"fixture operation is not required: {operation['name']}")
    if not operation['production_packet'].startswith('classic-op-'):
        raise SystemExit(f"operation packet is not focused: {operation['name']}")
    if not all(isinstance(source, str) and source.startswith('src/') for source in operation['primary_sources']):
        raise SystemExit(f"missing pinned production source for {operation['name']}")
    if operation['inventory_boundary'] not in inventory:
        raise SystemExit(f"missing inventory boundary reference for {operation['name']}")
    by_code[code] = operation

trace_codes = []
for fixture in ('trusted-classic-build-v1.json', 'untrusted-classic-build-v1.json'):
    path = traces_path / fixture
    if not path.is_file():
        raise SystemExit(f'missing trace artifact: {path}')
    trace = json.loads(path.read_text())
    if trace.get('operations') is None:
        raise SystemExit(f'missing operations in {path}')
    codes = set(trace['operations'])
    trace_codes.append(codes)

observed = set.intersection(*trace_codes)
manifest_codes = set(by_code)
uncovered = sorted(observed - manifest_codes)
extra = sorted(manifest_codes - observed)
if uncovered:
    raise SystemExit(f'uncovered required fixture operations: {uncovered}')
if extra:
    raise SystemExit(f'manifest includes operations absent from every accepted trace: {extra}')

for name, code in (("SetOptions", 19), ("IsValidPath", 1), ("AddToStore", 7),
                   ("QueryMissing", 40), ("QueryPathInfo", 26),
                   ("AddTempRoot", 11)):
    if f'| `{name}` | `{code}` |' not in allowlist and not (name == 'AddTempRoot' and f'`{name}` (`{code}`)' in allowlist):
        raise SystemExit(f'missing authoritative allowlist entry: {name} ({code})')

walking = manifest.get('walking_skeleton_operations')
if not isinstance(walking, list) or not walking:
    raise SystemExit('manifest has no walking-skeleton operations')
walking_by_code = {}
for operation in walking:
    required = (
        'name', 'code', 'fixture_role', 'admission', 'production_packet',
        'decoder', 'dispatcher', 'store_behavior', 'response_framing',
        'compatibility_test', 'primary_sources', 'inventory_boundary'
    )
    missing = [field for field in required if not operation.get(field)]
    if missing:
        raise SystemExit(f"walking-skeleton operation {operation.get('name', '<unnamed>')} missing fields: {', '.join(missing)}")
    code = operation['code']
    if not isinstance(code, int) or code < 0 or code in walking_by_code or code in by_code:
        raise SystemExit(f'invalid or duplicate walking-skeleton operation code: {code!r}')
    if not operation['production_packet'].startswith('classic-op-'):
        raise SystemExit(f"walking-skeleton packet is not focused: {operation['name']}")
    if not all(isinstance(source, str) and source.startswith('src/') for source in operation['primary_sources']):
        raise SystemExit(f"missing pinned production source for walking-skeleton operation {operation['name']}")
    if operation['inventory_boundary'] not in inventory:
        raise SystemExit(f"missing walking-skeleton inventory boundary reference for {operation['name']}")
    walking_by_code[code] = operation

build_derivation = walking_by_code.get(36)
if not build_derivation or build_derivation['name'] != 'BuildDerivation':
    raise SystemExit('walking skeleton must map BuildDerivation (36)')

if manifest.get('rejected_fixture_flows') == []:
    raise SystemExit('manifest must preserve rejected fixture-flow boundary')

print(f'classic-build operation coverage check passed: {len(observed)} trace operations, {len(walking)} walking-skeleton operations, uncovered=0')
PY
