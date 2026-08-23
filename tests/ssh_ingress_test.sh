#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
entrypoint="$repository_root/deploy/ssh/telchar-ssh-ingress.sh"
forced_command="$repository_root/deploy/ssh/telchar-ssh-forced-command.sh"

grep -q 'TELCHAR_SSH_HOST_IDENTITY_MODE' "$entrypoint"
grep -q 'TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE' "$entrypoint"
grep -q 'TELCHAR_SSH_AUTHORIZED_KEYS_FILE' "$entrypoint"

temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT

bin_directory="$temporary_directory/bin"
mkdir -p "$bin_directory"

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

wait_for_log() {
  local pattern="$1"
  local log_file="$2"
  for _ in $(seq 1 100); do
    grep -q -- "$pattern" "$log_file" 2>/dev/null && return
    /bin/sleep 0.01
  done
  printf 'missing log pattern: %s\n' "$pattern" >&2
  exit 1
}

run_mode() {
  local host_mode="$1"
  local client_mode="$2"
  local changed_file="$3"
  local case_directory="$temporary_directory/$host_mode-$client_mode"
  local credential_directory="$case_directory/credentials"
  local log_file="$case_directory/commands.log"
  mkdir -p "$credential_directory"

  local host_key="$credential_directory/host-key"
  local host_certificate="$credential_directory/host-certificate"
  local client_ca="$credential_directory/client-ca"
  local authorized_keys="$credential_directory/authorized-keys"
  printf 'key-a\n' >"$host_key"
  printf 'certificate-a\n' >"$host_certificate"
  printf 'ca-a\n' >"$client_ca"
  printf 'authorized-key-a\n' >"$authorized_keys"
  printf 'test configuration\n' >"$case_directory/sshd_config"

  TEST_LOG="$log_file" \
  PATH="$bin_directory:/bin:/usr/bin" \
  TELCHAR_SSH_HOST_IDENTITY_MODE="$host_mode" \
  TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE="$client_mode" \
  TELCHAR_SSH_HOST_KEY_FILE="$host_key" \
  TELCHAR_SSH_HOST_CERTIFICATE_FILE="$host_certificate" \
  TELCHAR_SSH_CLIENT_CA_FILE="$client_ca" \
  TELCHAR_SSH_AUTHORIZED_KEYS_FILE="$authorized_keys" \
  TELCHAR_SSH_CREDENTIAL_POLL_SECONDS=1 \
  TELCHAR_SSHD_CONFIG="$case_directory/sshd_config" \
    bash "$entrypoint" &
  local entrypoint_pid=$!
  trap 'kill -TERM "$entrypoint_pid" 2>/dev/null || true; wait "$entrypoint_pid" 2>/dev/null || true' RETURN

  wait_for_log 'sshd -D' "$log_file"
  grep -q -- "-o HostKey=$host_key" "$log_file"
  if [[ "$host_mode" == certificate ]]; then
    grep -q -- "-o HostCertificate=$host_certificate" "$log_file"
  else
    ! grep -q -- 'HostCertificate=' "$log_file"
  fi
  if [[ "$client_mode" == certificate ]]; then
    grep -q -- "-o TrustedUserCAKeys=$client_ca" "$log_file"
    ! grep -q -- 'AuthorizedKeysFile=' "$log_file"
  else
    grep -q -- "-o AuthorizedKeysFile=$authorized_keys" "$log_file"
    ! grep -q -- 'TrustedUserCAKeys=' "$log_file"
  fi

  /bin/sleep 0.15
  ! grep -q '^hup$' "$log_file"
  printf 'changed\n' >>"$credential_directory/$changed_file"
  wait_for_log '^hup$' "$log_file"
  [[ "$(grep -c '^hup$' "$log_file")" -eq 1 ]]

  kill -TERM "$entrypoint_pid"
  wait "$entrypoint_pid"
  trap - RETURN
}

run_mode key authorized-keys authorized-keys
run_mode certificate certificate host-certificate

set +e
invalid_output="$({
  TEST_LOG="$temporary_directory/invalid.log" \
  PATH="$bin_directory:/bin:/usr/bin" \
  TELCHAR_SSH_HOST_IDENTITY_MODE=invalid \
  TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE=authorized-keys \
  TELCHAR_SSHD_CONFIG="$temporary_directory/missing" \
    bash "$entrypoint"
} 2>&1)"
invalid_status=$?
set -e
[[ "$invalid_status" -ne 0 ]]
grep -q 'TELCHAR_SSH_HOST_IDENTITY_MODE must be key or certificate' <<<"$invalid_output"

grep -q 'ssh-keygen -lf -' "$forced_command"
grep -q 'ssh-keygen -L -f' "$forced_command"

if grep -Eqi 'vault|curl|TELCHAR_SSH_HOST_SIGN_PATH|TELCHAR_SSH_HOST_PRINCIPALS' "$entrypoint"; then
  printf 'SSH ingress entrypoint contains credential-provider-specific behavior\n' >&2
  exit 1
fi

printf 'PASS: SSH ingress supports explicit host and client authentication modes.\n'
