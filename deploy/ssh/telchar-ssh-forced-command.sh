#!/usr/bin/env bash
set -euo pipefail

: "${SSH_USER_AUTH:?OpenSSH authentication metadata is unavailable}"
: "${TELCHAR_IPC_SOCKET:?TELCHAR_IPC_SOCKET is required}"

certificate_file="$(mktemp)"
trap 'rm -f "$certificate_file"' EXIT

awk '$1 == "publickey" && $2 == "ssh-ed25519-cert-v01@openssh.com" { print $2, $3; exit }' \
	"$SSH_USER_AUTH" >"$certificate_file"
if [[ ! -s "$certificate_file" ]]; then
	echo "Telchar requires an OpenSSH user certificate" >&2
	exit 1
fi

certificate_details="$(ssh-keygen -L -f "$certificate_file")"
ca_fingerprint="$(printf '%s\n' "$certificate_details" | awk '/Signing CA:/ { print $4; exit }')"
key_id="$(printf '%s\n' "$certificate_details" | awk -F'"' '/Key ID:/ { print $2; exit }')"
if [[ -z "$ca_fingerprint" || -z "$key_id" ]]; then
	echo "OpenSSH certificate identity is incomplete" >&2
	exit 1
fi

exec env \
	TELCHAR_AUTHENTICATED_KEY="${ca_fingerprint}:${key_id}" \
	TELCHAR_IPC_SOCKET="$TELCHAR_IPC_SOCKET" \
	/bin/telchar serve-stdio
