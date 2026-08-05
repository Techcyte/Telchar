#!/bin/sh
set -eu

inventory='docs/protocol-fixture-flow-inventory.md'

require() {
	text=$1
	if ! grep -F -- "$text" "$inventory" >/dev/null; then
		printf 'missing required fixture-flow inventory text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$inventory" ] || {
	printf 'missing fixture-flow inventory: %s\n' "$inventory" >&2
	exit 1
}

for fixture in \
	'crates/telchar/tests/stdio_handshake.rs' \
	'crates/telchar/tests/worker_trace.rs'; do
	require "$fixture"
done

for boundary in \
	'WORKER_MAGIC_1' \
	'WORKER_MAGIC_2' \
	'`SetOptions` operation `19`' \
	'`STDERR_LAST`' \
	'`>= 1.38`' \
	'256 override pairs' \
	'16384 bytes per name or value'; do
	require "$boundary"
done

for source in \
	'src/libutil/include/nix/util/serialise.hh' \
	'src/libutil/serialise.cc' \
	'src/libstore/worker-protocol-connection.cc' \
	'src/libstore/worker-protocol.cc' \
	'src/libstore/remote-store.cc' \
	'src/libstore/daemon.cc'; do
	require "$source"
done

for unsupported in \
	'Worker operation other than `SetOptions`' \
	'Callback (`STDERR_READ` or `STDERR_WRITE`)' \
	'Upload (`AddToStore`, `AddToStoreNar`, `AddMultipleToStore`)' \
	'Activity/error/result frame other than `STDERR_LAST`'; do
	require "$unsupported"
done

printf 'protocol fixture-flow inventory check passed\n'
