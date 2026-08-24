#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image_archive="${TELCHAR_SSH_INGRESS_IMAGE_ARCHIVE:-}"
image="${TELCHAR_SSH_INGRESS_IMAGE:-}"
container_name="telchar-ssh-ingress-test-$$"
temporary_directory="$(mktemp -d "$repository_root/.ssh-ingress-test.XXXXXX")"

cleanup() {
  docker rm -f "$container_name" >/dev/null 2>&1 || true
  if [[ -n "${image:-}" && -d "$temporary_directory" ]]; then
    docker run --rm --entrypoint /bin/chown -v "$temporary_directory:/test-cleanup" "$image" \
      -R "$(id -u):$(id -g)" /test-cleanup >/dev/null 2>&1 || true
  fi
  rm -rf "$temporary_directory"
}
trap cleanup EXIT

if [[ -z "$image_archive" ]]; then
  image_archive="$(
    cd "$repository_root"
    NIXPKGS_ALLOW_UNFREE=1 nix build --impure --no-link --print-out-paths .#telchar-ssh-ingress-oci
  )"
fi

if [[ -z "$image" ]]; then
  load_output="$(docker load <"$image_archive")"
  image="${load_output##*Loaded image: }"
  [[ "$image" != "$load_output" ]] || {
    printf 'docker load did not report an image tag: %s\n' "$load_output" >&2
    exit 1
  }
else
  docker load <"$image_archive" >/dev/null
fi

credentials="$temporary_directory/credentials"
mkdir -p "$credentials" "$temporary_directory/run"
ssh-keygen -q -t ed25519 -N '' -f "$credentials/host-ca"
ssh-keygen -q -t ed25519 -N '' -f "$credentials/client-ca"
ssh-keygen -q -t ed25519 -N '' -f "$credentials/host"
ssh-keygen -q -t ed25519 -N '' -f "$credentials/client"
ssh-keygen -q -s "$credentials/host-ca" -I host-initial -h -n telchar.service.consul -V +1h "$credentials/host.pub"
ssh-keygen -q -s "$credentials/client-ca" -I client -n nix-builder -V +1h "$credentials/client.pub"
chmod 0600 "$credentials/host"
chmod 0644 "$credentials/host-cert.pub" "$credentials/client-ca.pub"

cat >"$temporary_directory/sshd_config" <<'EOF'
Port 2222
ListenAddress 0.0.0.0
PidFile /tmp/sshd.pid
AuthenticationMethods publickey
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
StrictModes yes
ExposeAuthInfo yes
UsePAM no
AllowUsers telchar
ForceCommand /bin/sh -c 'env; cat "$SSH_USER_AUTH"'
DisableForwarding yes
PermitTTY no
UseDNS no
LogLevel VERBOSE
EOF

docker run -d --name "$container_name" \
  -p 127.0.0.1::2222 \
  -e TELCHAR_SSH_HOST_IDENTITY_MODE=certificate \
  -e TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE=certificate \
  -e TELCHAR_SSH_HOST_KEY_FILE=/credentials/host \
  -e TELCHAR_SSH_HOST_CERTIFICATE_FILE=/credentials/host-cert.pub \
  -e TELCHAR_SSH_CLIENT_CA_FILE=/credentials/client-ca.pub \
  -e TELCHAR_SSH_AUTHORIZED_PRINCIPAL=nix-builder \
  -e TELCHAR_SSH_CREDENTIAL_POLL_SECONDS=1 \
  -e TELCHAR_SSHD_CONFIG=/test/sshd_config \
  -e TELCHAR_IPC_SOCKET=/test/daemon.sock \
  -v "$credentials:/credentials" \
  -v "$temporary_directory/sshd_config:/test/sshd_config:ro" \
  "$image" >/dev/null

fail() {
  docker inspect -f 'container running={{.State.Running}} exit={{.State.ExitCode}}' "$container_name" >&2
  docker logs "$container_name" >&2
  exit 1
}

for _ in $(seq 1 50); do
  [[ "$(docker inspect -f '{{.State.Running}}' "$container_name")" == true ]] || {
    docker logs "$container_name" >&2
    exit 1
  }
  port="$(docker port "$container_name" 2222/tcp | awk -F: 'NR == 1 { print $NF }')"
  [[ -n "$port" ]] && break
  sleep 0.1
done

known_hosts="$temporary_directory/known_hosts"
printf '@cert-authority telchar.service.consul %s\n' "$(cat "$credentials/host-ca.pub")" >"$known_hosts"
ssh_options=(
  -F /dev/null
  -o BatchMode=yes
  -o ConnectTimeout=5
  -o HostKeyAlias=telchar.service.consul
  -o IdentitiesOnly=yes
  -o "IdentityFile=$credentials/client"
  -o "CertificateFile=$credentials/client-cert.pub"
  -o "UserKnownHostsFile=$known_hosts"
  -o LogLevel=ERROR
  -o RequestTTY=no
  -p "$port"
)
set +e
session_environment="$(ssh "${ssh_options[@]}" telchar@127.0.0.1 </dev/null)"
ssh_status=$?
set -e
[[ "$ssh_status" -eq 0 ]] || fail
grep -qx 'TELCHAR_IPC_SOCKET=/test/daemon.sock' <<<"$session_environment" || fail
grep -q '^publickey ssh-ed25519-cert-v01@openssh.com ' <<<"$session_environment" || fail

sleep 2
idle_logs="$(docker logs "$container_name" 2>&1)"
if grep -q 'Received SIGHUP; restarting.' <<<"$idle_logs"; then
  fail
fi
ssh-keygen -q -s "$credentials/host-ca" -I host-rotated -h -n telchar.service.consul -V +2h "$credentials/host.pub"
reloaded=false
for _ in $(seq 1 50); do
  current_logs="$(docker logs "$container_name" 2>&1)"
  if grep -q 'Received SIGHUP; restarting.' <<<"$current_logs"; then
    reloaded=true
    break
  fi
  sleep 0.1
done
[[ "$reloaded" == true ]] || fail
ssh "${ssh_options[@]}" telchar@127.0.0.1 </dev/null >/dev/null || fail
[[ "$(docker inspect -f '{{.State.Running}}' "$container_name")" == true ]] || fail

docker rm -f "$container_name" >/dev/null
container_name="telchar-ssh-ingress-static-test-$$"
printf '%s\n' "$(cat "$credentials/client.pub")" >"$credentials/authorized_keys"
chmod 0600 "$credentials/authorized_keys"
docker run --rm --entrypoint /bin/chown -v "$credentials:/credentials" "$image" \
  995:995 /credentials /credentials/authorized_keys
docker run -d --name "$container_name" \
  -p 127.0.0.1::2222 \
  -e TELCHAR_SSH_HOST_IDENTITY_MODE=key \
  -e TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE=authorized-keys \
  -e TELCHAR_SSH_HOST_KEY_FILE=/credentials/host \
  -e TELCHAR_SSH_AUTHORIZED_KEYS_FILE=/credentials/authorized_keys \
  -e TELCHAR_SSH_CREDENTIAL_POLL_SECONDS=1 \
  -e TELCHAR_SSHD_CONFIG=/test/sshd_config \
  -e TELCHAR_IPC_SOCKET=/test/daemon.sock \
  -v "$credentials:/credentials" \
  -v "$temporary_directory/sshd_config:/test/sshd_config:ro" \
  "$image" >/dev/null

for _ in $(seq 1 50); do
  [[ "$(docker inspect -f '{{.State.Running}}' "$container_name")" == true ]] || fail
  port="$(docker port "$container_name" 2222/tcp | awk -F: 'NR == 1 { print $NF }')"
  [[ -n "$port" ]] && break
  sleep 0.1
done
printf 'telchar-static %s\n' "$(cat "$credentials/host.pub")" >"$known_hosts"
static_ssh_options=(
  -F /dev/null
  -o BatchMode=yes
  -o ConnectTimeout=5
  -o HostKeyAlias=telchar-static
  -o IdentitiesOnly=yes
  -o "IdentityFile=$credentials/client"
  -o "UserKnownHostsFile=$known_hosts"
  -o LogLevel=ERROR
  -o RequestTTY=no
  -p "$port"
)
set +e
static_session_environment="$(ssh "${static_ssh_options[@]}" telchar@127.0.0.1 </dev/null)"
static_ssh_status=$?
set -e
[[ "$static_ssh_status" -eq 0 ]] || fail
grep -qx 'TELCHAR_IPC_SOCKET=/test/daemon.sock' <<<"$static_session_environment" || fail
grep -q '^publickey ssh-ed25519 ' <<<"$static_session_environment" || fail
if grep -q -- '-cert-v01@openssh.com' <<<"$static_session_environment"; then
  fail
fi

printf 'PASS: SSH ingress authenticates in certificate and static-key modes and reloads once after certificate rotation.\n'
