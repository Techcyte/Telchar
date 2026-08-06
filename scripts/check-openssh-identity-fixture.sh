#!/bin/sh
set -eu

root=$(mktemp -d "${TMPDIR:-/tmp}/telchar-identity.XXXXXX")
pid=''
cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  rm -rf "$root"
}
trap cleanup EXIT INT TERM

umask 077
mkdir -p "$root/.ssh"
port=$((20000 + (PPID % 20000)))
ssh-keygen -q -t ed25519 -N '' -f "$root/client-key"
ssh-keygen -q -t ed25519 -N '' -f "$root/host-key"
fingerprint=$(ssh-keygen -lf "$root/client-key.pub" | awk '{print $2}')
cat >"$root/forced-command.sh" <<'EOF'
#!/bin/sh
set -eu
printf 'authenticated_key=%s\n' "$TELCHAR_AUTHENTICATED_KEY" >"$TELCHAR_IDENTITY_OUTPUT"
printf 'client_supplied_key=%s\n' "${TELCHAR_CLIENT_SUPPLIED_KEY-}" >>"$TELCHAR_IDENTITY_OUTPUT"
printf 'original_command=%s\n' "${SSH_ORIGINAL_COMMAND-}" >>"$TELCHAR_IDENTITY_OUTPUT"
printf 'authenticated_match=%s\n' "$([ "$TELCHAR_AUTHENTICATED_KEY" = "$TELCHAR_EXPECTED_KEY" ] && echo yes || echo no)" >>"$TELCHAR_IDENTITY_OUTPUT"
printf 'client_spoof_match=%s\n' "$([ "${TELCHAR_CLIENT_SUPPLIED_KEY-}" = spoofed ] && echo yes || echo no)" >>"$TELCHAR_IDENTITY_OUTPUT"
EOF
chmod 700 "$root/forced-command.sh"
cat >"$root/authorized_keys" <<EOF
command="env TELCHAR_AUTHENTICATED_KEY=$fingerprint TELCHAR_EXPECTED_KEY=$fingerprint TELCHAR_IDENTITY_OUTPUT=$root/identity.env $root/forced-command.sh",no-pty,no-agent-forwarding,no-X11-forwarding,no-port-forwarding $(<"$root/client-key.pub")
EOF
cat >"$root/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $root/host-key
PidFile $root/sshd.pid
AuthorizedKeysFile $root/authorized_keys
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
PermitEmptyPasswords no
UsePAM no
PermitUserEnvironment no
AllowTcpForwarding no
AllowAgentForwarding no
X11Forwarding no
PermitTTY no
LogLevel VERBOSE
ExposeAuthInfo yes
SetEnv TELCHAR_IDENTITY_OUTPUT=$root/identity.env
EOF

sshd_bin=${SSHD_BIN:-/run/current-system/sw/bin/sshd}
ssh_bin=${SSH_BIN:-/run/current-system/sw/bin/ssh}
"$sshd_bin" -t -f "$root/sshd_config"
"$sshd_bin" -D -e -f "$root/sshd_config" -o "PidFile=$root/sshd.pid" >"$root/sshd.log" 2>&1 &
pid=$!
for _ in $(seq 1 100); do
  [ -s "$root/sshd.pid" ] && break
  kill -0 "$pid" 2>/dev/null || { cat "$root/sshd.log" >&2; exit 1; }
  sleep 0.01
done
[ -s "$root/sshd.pid" ] || { cat "$root/sshd.log" >&2; exit 1; }
ssh_args="-o StrictHostKeyChecking=no -o UserKnownHostsFile=$root/known_hosts -o IdentitiesOnly=yes -o SendEnv=TELCHAR_CLIENT_SUPPLIED_KEY -p $port -i $root/client-key"
TELCHAR_CLIENT_SUPPLIED_KEY=spoofed "$ssh_bin" -q $ssh_args "$(id -un)@127.0.0.1" ignored || {
  cat "$root/sshd.log" >&2
  exit 1
}

grep -F "authenticated_key=$fingerprint" "$root/identity.env" >/dev/null
grep -F 'authenticated_match=yes' "$root/identity.env" >/dev/null
grep -F 'client_supplied_key=' "$root/identity.env" >/dev/null
grep -F 'client_spoof_match=no' "$root/identity.env" >/dev/null
grep -F 'original_command=' "$root/identity.env" >/dev/null
printf 'OpenSSH public-key identity handoff passed: fingerprint %s; client spoof rejected\n' "$fingerprint"
