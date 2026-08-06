#!/bin/sh
set -eu
adr='docs/adr/openssh-process-ipc-threat-model.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'
[ -f "$adr" ] || {
	echo "missing ADR: $adr" >&2
	exit 1
}
for text in \
	'OpenSSH is the network-facing authentication and transport boundary' \
	'per-connection forced command' \
	'unprivileged' \
	'dedicated local IPC endpoint' \
	'PostgreSQL' \
	'gateway store' \
	'authenticated public-key fingerprint' \
	'not client-supplied environment' \
	'SO_PEERCRED' \
	'wrong-user fixture' \
	'SSH_CONNECTION' \
	'Spoofing and abuse threats' \
	'Telemetry boundary and redaction obligations'; do
	grep -F -- "$text" "$adr" >/dev/null || {
		echo "missing threat-model evidence: $text" >&2
		exit 1
	}
done
grep -F -- 'T046 Document OpenSSH process and IPC threat model' "$plan" >/dev/null || {
	echo 'master plan does not define T046' >&2
	exit 1
}
echo 'OpenSSH process and IPC threat-model checklist passed'
