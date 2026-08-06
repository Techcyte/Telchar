#!/bin/sh
set -eu

root=$(mktemp -d "${TMPDIR:-/tmp}/telchar-openssh.XXXXXX")
pid=''
cleanup() {
	if [ -n "$pid" ]; then
		kill "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
	fi
	rm -rf "$root"
}
trap cleanup EXIT INT TERM

umask 077
mkdir -p "$root/.ssh"
port=$((20000 + (PPID % 20000)))
sshd_bin=${SSHD_BIN:-"$(command -v sshd)"}
ssh_bin=${SSH_BIN:-"$(command -v ssh)"}
ssh_keygen_bin=${SSH_KEYGEN_BIN:-"$(command -v ssh-keygen)"}

"$ssh_keygen_bin" -q -t ed25519 -N '' -f "$root/host-key"
"$ssh_keygen_bin" -q -t ed25519 -N '' -f "$root/client-key"
fingerprint=$($ssh_keygen_bin -lf "$root/client-key.pub" | awk '{print $2}')
cat >"$root/forced-command.sh" <<'EOF'
#!/bin/sh
set -eu
printf 'telchar-forced-command\n'
printf 'authenticated_key=%s\n' "$TELCHAR_AUTHENTICATED_KEY" >"$TELCHAR_FIXTURE_OUTPUT"
printf 'original_command=%s\n' "${SSH_ORIGINAL_COMMAND-}" >>"$TELCHAR_FIXTURE_OUTPUT"
EOF
chmod 700 "$root/forced-command.sh"
cat >"$root/authorized_keys" <<EOF
command="env TELCHAR_AUTHENTICATED_KEY=$fingerprint TELCHAR_FIXTURE_OUTPUT=$root/forced-command-output $root/forced-command.sh",no-pty,no-agent-forwarding,no-X11-forwarding,no-port-forwarding $(cat "$root/client-key.pub")
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
LogLevel ERROR
EOF

"$sshd_bin" -t -f "$root/sshd_config"
"$sshd_bin" -D -e -f "$root/sshd_config" >"$root/sshd.log" 2>&1 &
pid=$!
for _ in $(seq 1 100); do
	[ -s "$root/sshd.pid" ] && break
	kill -0 "$pid" 2>/dev/null || {
		cat "$root/sshd.log" >&2
		exit 1
	}
	sleep 0.01
done
[ -s "$root/sshd.pid" ] || {
	cat "$root/sshd.log" >&2
	exit 1
}

output=$(
	"$ssh_bin" -q \
		-o StrictHostKeyChecking=no \
		-o UserKnownHostsFile="$root/known_hosts" \
		-o IdentitiesOnly=yes \
		-p "$port" \
		-i "$root/client-key" \
		"$(id -un)@127.0.0.1" \
		'untrusted-command'
)
[ "$output" = 'telchar-forced-command' ]
grep -F "authenticated_key=$fingerprint" "$root/forced-command-output" >/dev/null
grep -F 'original_command=untrusted-command' "$root/forced-command-output" >/dev/null

printf 'OpenSSH fixture passed: port=%s host_key=%s client_key=%s forced_command=%s cleanup=automatic\n' \
	"$port" "$root/host-key" "$root/client-key" "$root/forced-command.sh"
