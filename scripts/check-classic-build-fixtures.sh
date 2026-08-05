#!/bin/sh
set -eu

contract='docs/classic-build-fixtures.md'

require() {
	text=$1
	if ! grep -F -- "$text" "$contract" >/dev/null; then
		printf 'missing required classic-build fixture contract text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$contract" ] || {
	printf 'missing classic-build fixture contract: %s\n' "$contract" >&2
	exit 1
}

for text in \
	'## Common deterministic derivation' \
	'`984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f`' \
	'`printf telchar-classic-fixture > "$out"`' \
	'## Trusted fixture' \
	'`trusted-users = travis`' \
	'`"trusted":true`' \
	'## Untrusted fixture' \
	'`trusted-users = root`' \
	'`"trusted":false`' \
	'`NIX_STORE_DIR`' \
	'`NIX_STATE_DIR`' \
	'`NIX_DAEMON_SOCKET_PATH`' \
	'`NIX_USER_CONF_FILES=/dev/null`' \
	'`--store unix://<fixture-socket>`' \
	'`build-hook =`' \
	'`substituters =`' \
	'`--no-link --print-out-paths`' \
	'Client local-build prohibition' \
	'no local store path is configured for the client' \
	'`NixDaemon::stop`' \
	'`NixFixture::cleanup`'; do
	require "$text"
done

printf 'classic-build fixture contract check passed\n'
