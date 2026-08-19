#!/usr/bin/env bash
set -euo pipefail

: "${VAULT_ADDR:?VAULT_ADDR is required}"
: "${VAULT_TOKEN:?VAULT_TOKEN is required}"
: "${TELCHAR_SSH_HOST_SIGN_PATH:?TELCHAR_SSH_HOST_SIGN_PATH is required}"
: "${TELCHAR_SSH_HOST_PRINCIPALS:?TELCHAR_SSH_HOST_PRINCIPALS is required}"

identity_directory="${TELCHAR_SSH_IDENTITY_DIRECTORY:-/var/lib/telchar-ssh}"
host_key="$identity_directory/ssh_host_ed25519_key"
host_certificate="$identity_directory/ssh_host_ed25519_key-cert.pub"
client_ca="${TELCHAR_SSH_CLIENT_CA_FILE:-$identity_directory/client-ca.pub}"
refresh_seconds="${TELCHAR_SSH_CERT_REFRESH_SECONDS:-43200}"
sshd_config="${TELCHAR_SSHD_CONFIG:-/etc/ssh/sshd_config}"

install -d -m 0700 "$identity_directory"
if [[ ! -f "$host_key" ]]; then
	ssh-keygen -q -t ed25519 -N "" -f "$host_key"
fi
chmod 0600 "$host_key"
chmod 0644 "$host_key.pub"

render_trust() {
	local temporary_ca temporary_certificate public_key request
	temporary_ca="$(mktemp "$identity_directory/client-ca.XXXXXX")"
	temporary_certificate="$(mktemp "$identity_directory/host-cert.XXXXXX")"
	public_key="$(<"$host_key.pub")"
	request="$(jq -nc \
		--arg public_key "$public_key" \
		--arg valid_principals "$TELCHAR_SSH_HOST_PRINCIPALS" \
		'{public_key: $public_key, cert_type: "host", valid_principals: $valid_principals}')"

	curl --fail --silent --show-error \
		--header "X-Vault-Token: $VAULT_TOKEN" \
		"$VAULT_ADDR/v1/ssh-nix-client/config/ca" |
		jq -er '.data.public_key' >"$temporary_ca"
	curl --fail --silent --show-error \
		--header "X-Vault-Token: $VAULT_TOKEN" \
		--header 'Content-Type: application/json' \
		--request POST \
		--data "$request" \
		"$VAULT_ADDR/v1/$TELCHAR_SSH_HOST_SIGN_PATH" |
		jq -er '.data.signed_key' >"$temporary_certificate"

	ssh-keygen -L -f "$temporary_certificate" >/dev/null
	chmod 0644 "$temporary_ca" "$temporary_certificate"
	mv -f "$temporary_ca" "$client_ca"
	mv -f "$temporary_certificate" "$host_certificate"
}

render_trust
sshd -t -f "$sshd_config"
sshd -D -e -f "$sshd_config" &
sshd_pid=$!

terminate() {
	kill -TERM "$sshd_pid" 2>/dev/null || true
	wait "$sshd_pid" || true
}
trap terminate TERM INT

while kill -0 "$sshd_pid" 2>/dev/null; do
	sleep "$refresh_seconds" &
	sleep_pid=$!
	wait "$sleep_pid" || true
	if ! kill -0 "$sshd_pid" 2>/dev/null; then
		break
	fi
	if render_trust; then
		kill -HUP "$sshd_pid"
	fi
done

wait "$sshd_pid"
