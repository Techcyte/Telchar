#!/bin/sh
set -eu

capture='docs/classic-build-diagnostic-capture.md'

require() {
	text=$1
	if ! grep -F -- "$text" "$capture" >/dev/null; then
		printf 'missing required diagnostic-capture text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$capture" ] || {
	printf 'missing diagnostic capture: %s\n' "$capture" >&2
	exit 1
}

for text in \
	'diagnostic discovery only — not compatibility acceptance evidence' \
	'`19, 11, 1, 7, 40, 26, 46`' \
	'`SetOptions`, `AddTempRoot`, `IsValidPath`, `AddToStore`, `QueryMissing`, `QueryPathInfo`, `BuildPathsWithResults`' \
	'Candidate equality demonstrates only' \
	'neither a NAR body, derivation body, output body, credential, socket path, nor unbounded string' \
	'`AddToStore` is a candidate upload boundary' \
	'No callback or response classification is accepted from debug text' \
	'src/libstore/remote-store.cc' \
	'src/libstore/daemon.cc::performOp' \
	'src/libstore/include/nix/store/worker-protocol.hh'; do
	require "$text"
done

printf 'classic-build diagnostic capture check passed\n'
