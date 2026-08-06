#!/bin/sh
set -eu
adr='docs/adr/supported-authenticated-identity-path.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'
[ -f "$adr" ] || {
	echo "missing ADR: $adr" >&2
	exit 1
}
for text in \
	'**Status:** Approved for initial ingress' \
	'OpenSSH-controlled identity path' \
	'authorized_keys' \
	'real fixture' \
	'client-supplied identity value cannot replace it' \
	'Source address alone' \
	'Gate 2 is not blocked' \
	'Certificate support remains explicitly deferred'; do
	grep -F -- "$text" "$adr" >/dev/null || {
		echo "missing supported identity evidence: $text" >&2
		exit 1
	}
done
sh scripts/check-openssh-identity-fixture.sh >/dev/null
grep -F -- 'T048A Approve supported authenticated identity path' "$plan" >/dev/null
echo 'supported authenticated identity-path checklist passed'
