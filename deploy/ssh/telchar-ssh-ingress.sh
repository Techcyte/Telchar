#!/usr/bin/env bash
set -euo pipefail

identity_directory="${TELCHAR_SSH_IDENTITY_DIRECTORY:-/var/lib/telchar-ssh}"
host_identity_mode="${TELCHAR_SSH_HOST_IDENTITY_MODE:-key}"
client_authentication_mode="${TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE:-authorized-keys}"
host_key="${TELCHAR_SSH_HOST_KEY_FILE:-$identity_directory/ssh_host_ed25519_key}"
host_certificate="${TELCHAR_SSH_HOST_CERTIFICATE_FILE:-$identity_directory/ssh_host_ed25519_key-cert.pub}"
client_ca="${TELCHAR_SSH_CLIENT_CA_FILE:-$identity_directory/client-ca.pub}"
authorized_keys="${TELCHAR_SSH_AUTHORIZED_KEYS_FILE:-$identity_directory/authorized_keys}"
poll_seconds="${TELCHAR_SSH_CREDENTIAL_POLL_SECONDS:-30}"
sshd_config="${TELCHAR_SSHD_CONFIG:-/etc/ssh/sshd_config}"

case "$host_identity_mode" in
	key)
		host_files=("$host_key")
		host_options=(-o "HostKey=$host_key" -o "HostCertificate=none")
		;;
	certificate)
		host_files=("$host_key" "$host_certificate")
		host_options=(-o "HostKey=$host_key" -o "HostCertificate=$host_certificate")
		;;
	*)
		echo "TELCHAR_SSH_HOST_IDENTITY_MODE must be key or certificate" >&2
		exit 1
		;;
esac

case "$client_authentication_mode" in
	authorized-keys)
		client_files=("$authorized_keys")
		client_options=(-o "AuthorizedKeysFile=$authorized_keys" -o "TrustedUserCAKeys=none")
		;;
	certificate)
		client_files=("$client_ca")
		client_options=(-o "AuthorizedKeysFile=none" -o "TrustedUserCAKeys=$client_ca")
		;;
	*)
		echo "TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE must be authorized-keys or certificate" >&2
		exit 1
		;;
esac

credential_files=("${host_files[@]}" "${client_files[@]}")
sshd_options=("${host_options[@]}" "${client_options[@]}" -f "$sshd_config")

credential_digest() {
	sha256sum "${credential_files[@]}"
}

sshd -t "${sshd_options[@]}"
credential_state="$(credential_digest)"
sshd -D -e "${sshd_options[@]}" &
sshd_pid=$!

terminate() {
	kill -TERM "$sshd_pid" 2>/dev/null || true
	wait "$sshd_pid" || true
}
trap terminate TERM INT

while kill -0 "$sshd_pid" 2>/dev/null; do
	sleep "$poll_seconds" &
	sleep_pid=$!
	wait "$sleep_pid" || true
	if ! kill -0 "$sshd_pid" 2>/dev/null; then
		break
	fi
	updated_credential_state="$(credential_digest)"
	if [[ "$updated_credential_state" != "$credential_state" ]]; then
		sshd -t "${sshd_options[@]}"
		kill -HUP "$sshd_pid"
		credential_state="$updated_credential_state"
	fi
done

wait "$sshd_pid"
