#!/usr/bin/env bash
set -euo pipefail

identity_directory="${TELCHAR_SSH_IDENTITY_DIRECTORY:-/var/lib/telchar-ssh}"
host_key="${TELCHAR_SSH_HOST_KEY_FILE:-$identity_directory/ssh_host_ed25519_key}"
host_certificate="${TELCHAR_SSH_HOST_CERTIFICATE_FILE:-$identity_directory/ssh_host_ed25519_key-cert.pub}"
client_ca="${TELCHAR_SSH_CLIENT_CA_FILE:-$identity_directory/client-ca.pub}"
poll_seconds="${TELCHAR_SSH_CREDENTIAL_POLL_SECONDS:-30}"
sshd_config="${TELCHAR_SSHD_CONFIG:-/etc/ssh/sshd_config}"

credential_digest() {
	sha256sum "$host_key" "$host_certificate" "$client_ca"
}

sshd -t \
	-o "HostKey=$host_key" \
	-o "HostCertificate=$host_certificate" \
	-o "TrustedUserCAKeys=$client_ca" \
	-f "$sshd_config"
credential_state="$(credential_digest)"
sshd -D -e \
	-o "HostKey=$host_key" \
	-o "HostCertificate=$host_certificate" \
	-o "TrustedUserCAKeys=$client_ca" \
	-f "$sshd_config" &
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
		sshd -t \
			-o "HostKey=$host_key" \
			-o "HostCertificate=$host_certificate" \
			-o "TrustedUserCAKeys=$client_ca" \
			-f "$sshd_config"
		kill -HUP "$sshd_pid"
		credential_state="$updated_credential_state"
	fi
done

wait "$sshd_pid"
