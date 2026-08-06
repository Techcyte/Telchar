#!/bin/sh
set -eu
adr='docs/adr/requester-normalization.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'
[ -f "$adr" ] || {
	echo "missing ADR: $adr" >&2
	exit 1
}
for text in \
	'Credential ID' \
	'Audit subject' \
	'Quota subject' \
	'Certificate metadata' \
	'Source address' \
	'ssh-pubkey:F' \
	'quota subject' \
	'credential ID' \
	'length-prefix' \
	'collisions' \
	'deterministic' \
	'client-supplied environment variables' \
	'256 bytes'; do
	grep -F -- "$text" "$adr" >/dev/null || {
		echo "missing normalization evidence: $text" >&2
		exit 1
	}
done
nix develop -c cargo test -p telchar --test identity --locked
grep -F -- 'T049 Define requester normalization' "$plan" >/dev/null
echo 'requester normalization checklist passed'
