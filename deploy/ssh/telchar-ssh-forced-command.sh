#!/usr/bin/env bash
set -euo pipefail

: "${SSH_USER_AUTH:?OpenSSH authentication metadata is unavailable}"
: "${TELCHAR_IPC_SOCKET:?TELCHAR_IPC_SOCKET is required}"

authenticated_key="$(awk '$1 == "publickey" { print $2, $3; exit }' "$SSH_USER_AUTH")"
if [[ -z "$authenticated_key" ]]; then
	echo "OpenSSH public-key identity is unavailable" >&2
	exit 1
fi

key_type="${authenticated_key%% *}"
if [[ "$key_type" == *-cert-v01@openssh.com ]]; then
	certificate_file="$(mktemp)"
	trap 'rm -f "$certificate_file"' EXIT
	printf '%s\n' "$authenticated_key" >"$certificate_file"
	certificate_details="$(ssh-keygen -L -f "$certificate_file")"
	ca_fingerprint="$(printf '%s\n' "$certificate_details" | awk '/Signing CA:/ { print $4; exit }')"
	key_id="$(printf '%s\n' "$certificate_details" | awk -F'"' '/Key ID:/ { print $2; exit }')"
	if [[ -z "$ca_fingerprint" || -z "$key_id" ]]; then
		echo "OpenSSH certificate identity is incomplete" >&2
		exit 1
	fi
	identity="${ca_fingerprint}:${key_id}"
else
	identity="$(printf '%s\n' "$authenticated_key" | ssh-keygen -lf - | awk '{ print $2; exit }')"
	if [[ -z "$identity" ]]; then
		echo "OpenSSH public-key fingerprint is unavailable" >&2
		exit 1
	fi
fi

exec env \
	TELCHAR_AUTHENTICATED_KEY="$identity" \
	TELCHAR_IPC_SOCKET="$TELCHAR_IPC_SOCKET" \
	/bin/telchar serve-stdio
