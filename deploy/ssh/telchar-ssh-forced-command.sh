#!/usr/bin/env bash
set -euo pipefail

: "${SSH_USER_AUTH:?OpenSSH authentication metadata is unavailable}"
: "${TELCHAR_IPC_SOCKET:?TELCHAR_IPC_SOCKET is required}"

authenticated_key="$(awk '$1 == "publickey" { print $2, $3; exit }' "$SSH_USER_AUTH")"
if [[ -z "$authenticated_key" ]]; then
	echo "OpenSSH public-key identity is unavailable" >&2
	exit 1
fi

unset TELCHAR_AUTHENTICATED_KEY TELCHAR_AUTHENTICATED_CA TELCHAR_AUTHENTICATED_KEY_ID TELCHAR_AUTHENTICATED_PRINCIPALS
key_type="${authenticated_key%% *}"
if [[ "$key_type" == *-cert-v01@openssh.com ]]; then
	certificate_file="$(mktemp)"
	trap 'rm -f "$certificate_file"' EXIT
	printf '%s\n' "$authenticated_key" >"$certificate_file"
	certificate_details="$(ssh-keygen -L -f "$certificate_file")"
	ca_fingerprint="$(printf '%s\n' "$certificate_details" | awk '/Signing CA:/ { print $4; exit }')"
	key_id="$(printf '%s\n' "$certificate_details" | awk -F'"' '/Key ID:/ { print $2; exit }')"
	principals="$(printf '%s\n' "$certificate_details" | awk '/^[[:space:]]*Principals:/{inside=1; next} inside && /^[[:space:]]*Critical Options:/{exit} inside {sub(/^[[:space:]]+/, ""); if (length) print}')"
	if [[ -z "$ca_fingerprint" || -z "$key_id" || -z "$principals" ]]; then
		echo "OpenSSH certificate identity is incomplete" >&2
		exit 1
	fi
	export TELCHAR_AUTHENTICATED_CA="$ca_fingerprint"
	export TELCHAR_AUTHENTICATED_KEY_ID="$key_id"
	export TELCHAR_AUTHENTICATED_PRINCIPALS="$principals"
	rm -f "$certificate_file"
	trap - EXIT
else
	identity="$(printf '%s\n' "$authenticated_key" | ssh-keygen -lf - | awk '{ print $2; exit }')"
	if [[ -z "$identity" ]]; then
		echo "OpenSSH public-key fingerprint is unavailable" >&2
		exit 1
	fi
	export TELCHAR_AUTHENTICATED_KEY="$identity"
fi

exec "${TELCHAR_PROGRAM:-/bin/telchar}" serve-stdio
