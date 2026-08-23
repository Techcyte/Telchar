#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
entrypoint="$repository_root/deploy/ssh/telchar-ssh-ingress.sh"
temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT

bin_directory="$temporary_directory/bin"
credential_directory="$temporary_directory/credentials"
log_file="$temporary_directory/commands.log"
mkdir -p "$bin_directory" "$credential_directory"

cat >"$bin_directory/sshd" <<'EOF'
#!/usr/bin/env bash
printf 'sshd %s\n' "$*" >>"$TEST_LOG"
if [[ " $* " == *" -D "* ]]; then
  trap 'printf "hup\n" >>"$TEST_LOG"' HUP
  trap 'exit 0' TERM INT
  while true; do sleep 1; done
fi
EOF
chmod +x "$bin_directory/sshd"

cat >"$bin_directory/sleep" <<'EOF'
#!/usr/bin/env bash
/bin/sleep 0.05
EOF
chmod +x "$bin_directory/sleep"

host_key="$credential_directory/host-key"
host_certificate="$credential_directory/host-certificate"
client_ca="$credential_directory/client-ca"
printf 'key-a\n' >"$host_key"
printf 'certificate-a\n' >"$host_certificate"
printf 'ca-a\n' >"$client_ca"

export PATH="$bin_directory:/bin:/usr/bin"
export TEST_LOG="$log_file"
export TELCHAR_SSH_HOST_KEY_FILE="$host_key"
export TELCHAR_SSH_HOST_CERTIFICATE_FILE="$host_certificate"
export TELCHAR_SSH_CLIENT_CA_FILE="$client_ca"
export TELCHAR_SSH_CREDENTIAL_POLL_SECONDS=1
export TELCHAR_SSHD_CONFIG="$temporary_directory/sshd_config"
printf 'test configuration\n' >"$TELCHAR_SSHD_CONFIG"

bash "$entrypoint" &
entrypoint_pid=$!
trap 'kill -TERM "$entrypoint_pid" 2>/dev/null || true; wait "$entrypoint_pid" 2>/dev/null || true; rm -rf "$temporary_directory"' EXIT

for _ in $(seq 1 100); do
  grep -q 'sshd -D' "$log_file" 2>/dev/null && break
  /bin/sleep 0.01
done
grep -q 'sshd -t' "$log_file"
grep -q 'sshd -D' "$log_file"

/bin/sleep 0.15
if grep -q '^hup$' "$log_file"; then
  printf 'unchanged credentials triggered an sshd reload\n' >&2
  exit 1
fi

printf 'certificate-b\n' >"$host_certificate"
for _ in $(seq 1 100); do
  grep -q '^hup$' "$log_file" && break
  /bin/sleep 0.01
done
[[ "$(grep -c '^hup$' "$log_file")" -eq 1 ]]

/bin/sleep 0.15
[[ "$(grep -c '^hup$' "$log_file")" -eq 1 ]]

kill -TERM "$entrypoint_pid"
wait "$entrypoint_pid"
trap 'rm -rf "$temporary_directory"' EXIT

if grep -Eq 'vault|curl|TELCHAR_SSH_HOST_SIGN_PATH|TELCHAR_SSH_HOST_PRINCIPALS' "$entrypoint"; then
  printf 'SSH ingress entrypoint still contains Vault-specific behavior\n' >&2
  exit 1
fi

printf 'PASS: SSH ingress reloads only when configurable credential files change.\n'
