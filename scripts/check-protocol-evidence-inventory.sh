#!/bin/sh
set -eu

inventory='docs/protocol-evidence-inventory.md'

[ -f "$inventory" ] || {
	printf 'missing protocol evidence inventory: %s\n' "$inventory" >&2
	exit 1
}

require() {
	text=$1
	if ! grep -F -- "$text" "$inventory" >/dev/null; then
		printf 'missing protocol evidence inventory text: %s\n' "$text" >&2
		exit 1
	fi
}

for text in \
	'## Evidence policy' \
	'Rio contributes only architecture or test-category notes' \
	'## Required behavior inventory' \
	'`docs/compatibility-traces/trusted-classic-build-v1.json`' \
	'`docs/compatibility-traces/untrusted-classic-build-v1.json`' \
	'`src/libutil/include/nix/util/serialise.hh`' \
	'`src/libstore/worker-protocol-connection.cc`' \
	'`src/libstore/worker-protocol.cc`' \
	'`src/libstore/remote-store.cc::' \
	'`src/libstore/daemon.cc::' \
	'`SetOptions` (`19`)' \
	'`AddTempRoot` (`11`)' \
	'`IsValidPath` (`1`)' \
	'`AddToStore` (`7`)' \
	'`QueryMissing` (`40`)' \
	'`QueryPathInfo` (`26`)' \
	'`BuildPathsWithResults` (`46`)' \
	'T023–T030' \
	'T031–T035' \
	'T036' \
	'T036A–T036B' \
	'T036C and T036H' \
	'T012–T016' \
	'No required behavior relies on Rio implementation details.'; do
	require "$text"
done

printf 'protocol evidence inventory check passed\n'
