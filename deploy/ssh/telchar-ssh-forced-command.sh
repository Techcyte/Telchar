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
unset TELCHAR_AUTHENTICATED_KEY_TYPE TELCHAR_AUTHENTICATED_SERIAL TELCHAR_AUTHENTICATED_VALID_FROM TELCHAR_AUTHENTICATED_VALID_TO TELCHAR_AUTHENTICATED_USER_KEY
key_type="${authenticated_key%% *}"
if [[ "$key_type" == *-cert-v01@openssh.com ]]; then
	certificate_file="$(mktemp)"
	trap 'rm -f "$certificate_file"' EXIT
	printf '%s\n' "$authenticated_key" >"$certificate_file"
	certificate_details="$(LC_ALL=C TZ=UTC ssh-keygen -L -f "$certificate_file")"
	serial="$(printf '%s\n' "$certificate_details" | awk '/Serial:/ { print $2; exit }')"
	user_key="$(printf '%s\n' "$certificate_details" | awk '/Public key:/ { print $4; exit }')"
	validity="$(printf '%s\n' "$certificate_details" | awk '/Valid:/ { sub(/^[[:space:]]*Valid: /, ""); print; exit }')"
	if [[ "$validity" == forever ]]; then
		valid_from="always"
		valid_to="forever"
	else
		valid_from="$(printf '%s\n' "$validity" | awk '{ print $2 }')"
		valid_to="$(printf '%s\n' "$validity" | awk '{ print $4 }')"
	fi
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
	export TELCHAR_AUTHENTICATED_KEY_TYPE="${key_type%-cert-v01@openssh.com}"
	export TELCHAR_AUTHENTICATED_SERIAL="$serial"
	export TELCHAR_AUTHENTICATED_USER_KEY="$user_key"
	export TELCHAR_AUTHENTICATED_VALID_FROM="$valid_from"
	export TELCHAR_AUTHENTICATED_VALID_TO="$valid_to"
	rm -f "$certificate_file"
	trap - EXIT
else
	identity="$(printf '%s\n' "$authenticated_key" | ssh-keygen -lf - | awk '{ print $2; exit }')"
	if [[ -z "$identity" ]]; then
		echo "OpenSSH public-key fingerprint is unavailable" >&2
		exit 1
	fi
	export TELCHAR_AUTHENTICATED_KEY="$identity"
	export TELCHAR_AUTHENTICATED_KEY_TYPE="$key_type"
fi

exec "${TELCHAR_PROGRAM:-/bin/telchar}" serve-stdio
