#!/usr/bin/env bash
set -euo pipefail

image="${TELCHAR_DOCKER_NIX_IMAGE:-nixos/nix:2.34.3}"
base="$PWD/.tmp-telchar-local-docker-$PPID-$$"
root="$base/run"
rm -rf "$base"
mkdir -p "$root"
chmod 0700 "$root"
container="telchar-local-nix-$PPID-$$"
volume="telchar-local-store-$PPID-$$"
daemon_pid=""
frontend_pid=""

cleanup() {
	if [[ -n "$frontend_pid" ]]; then kill "$frontend_pid" 2>/dev/null || true; fi
	if [[ -n "$daemon_pid" ]]; then kill "$daemon_pid" 2>/dev/null || true; fi
	wait "$frontend_pid" 2>/dev/null || true
	wait "$daemon_pid" 2>/dev/null || true
	docker rm -f "$container" >/dev/null 2>&1 || true
	docker volume rm "$volume" >/dev/null 2>&1 || true
	rm -rf "$base"
}
trap cleanup EXIT INT TERM

socket_dir="$root/nix-daemon"
ipc_socket="$root/telchar.sock"
mkdir -p "$socket_dir"
chmod 0777 "$socket_dir"
if ! docker image inspect "$image" >/dev/null 2>&1; then
	docker pull "$image" >/dev/null
fi
docker volume create "$volume" >/dev/null
docker run -d \
	--name "$container" \
	--privileged \
	-e NIX_CONFIG='trusted-users = root 1000' \
	-v "$volume:/nix/store" \
	-v "$socket_dir:/nix/var/nix/daemon-socket" \
	"$image" \
	sh -lc 'mkdir -p /nix/var/nix/db /nix/var/log/nix/drvs /nix/var/nix/profiles/per-user/root; exec nix-daemon' \
	>/dev/null

for _ in $(seq 1 100); do
	[[ -S "$socket_dir/socket" ]] && break
	sleep 0.1
done
[[ -S "$socket_dir/socket" ]]

telchar="$(nix build --no-link --print-out-paths .#telchar | tail -n 1)"
export TELCHAR_GATEWAY_STORE_URI="unix://$socket_dir/socket?root=/"
TELCHAR_NIX="$(command -v nix)"
export TELCHAR_NIX
TELCHAR_SYSTEM="$(nix eval --raw --impure --expr builtins.currentSystem)"
export TELCHAR_SYSTEM
export TELCHAR_SUPPORTED_FEATURES=""
export NIX_CONFIG=$'post-build-hook =\nsubstituters ='
export TELCHAR_IPC_SOCKET="$ipc_socket"
export TELCHAR_AUTHENTICATED_KEY="SHA256:local-docker-alpha"

"$telchar/bin/telchar" daemon \
	--socket "$ipc_socket" \
	--frontend-uid "$(id -u)" \
	>"$root/daemon.stdout" 2>"$root/daemon.stderr" &
daemon_pid=$!
for _ in $(seq 1 100); do
	[[ -S "$ipc_socket" ]] && break
	sleep 0.05
done
if [[ ! -S "$ipc_socket" ]]; then
	cat "$root/daemon.stderr" >&2
	exit 1
fi

mkfifo "$root/request" "$root/response"
"$telchar/bin/telchar" serve-stdio \
	<"$root/request" >"$root/response" 2>"$root/frontend.stderr" &
frontend_pid=$!

exec 3>"$root/request" 4<"$root/response"
cat >"$root/client.py" <<'PY'
import os, struct, sys

def integer(value):
    return struct.pack('<Q', value)

def string(value):
    if isinstance(value, str):
        value = value.encode()
    return integer(len(value)) + value + b'\0' * ((8 - len(value) % 8) % 8)

root = sys.argv[1]
request = b''.join([
    integer(0x6e697863), integer(0x126), integer(0),
    integer(0), integer(0),
    integer(36),
    string('/nix/store/00000000000000000000000000000000-telchar-local-docker.drv'),
    integer(1), string('out'),
    string('/nix/store/11111111111111111111111111111111-telchar-local-docker'),
    string(''), string(''), integer(0),
    string(os.environ['TELCHAR_SYSTEM']), string('/bin/sh'),
    integer(2), string('-c'), string('printf telchar-local-docker > $out'),
    integer(4),
    string('builder'), string('/bin/sh'),
    string('name'), string('telchar-local-docker'),
    string('out'), string('/nix/store/11111111111111111111111111111111-telchar-local-docker'),
    string('system'), string(os.environ['TELCHAR_SYSTEM']),
    integer(0),
])
sys.stdout.buffer.write(request)
sys.stdout.buffer.flush()

read = sys.stdin.buffer.read

def read_exact(length):
    data = b''
    while len(data) < length:
        chunk = read(length - len(data))
        if not chunk:
            raise EOFError(f'expected {length} bytes, received {len(data)}')
        data += chunk
    return data

assert struct.unpack('<Q', read_exact(8))[0] == 0x6478696f
assert struct.unpack('<Q', read_exact(8))[0] == 0x126
assert struct.unpack('<Q', read_exact(8))[0] == 0

def read_string():
    length = struct.unpack('<Q', read_exact(8))[0]
    value = read_exact(length)
    read_exact((8 - length % 8) % 8)
    return value

assert read_string() == b'telchar'
assert struct.unpack('<Q', read_exact(8))[0] == 0
assert struct.unpack('<Q', read_exact(8))[0] == 0x616c7473
assert struct.unpack('<Q', read_exact(8))[0] == 0x616c7473
status = struct.unpack('<Q', read_exact(8))[0]
if status == 0x63787470:
    message = read_string()
    raise RuntimeError(f'worker error: {message.decode(errors="replace")}')
assert status == 0
assert read_string() == b''
for _ in range(4): assert struct.unpack('<Q', read_exact(8))[0] == 0
assert struct.unpack('<Q', read_exact(8))[0] == 0
assert struct.unpack('<Q', read_exact(8))[0] == 0
assert struct.unpack('<Q', read_exact(8))[0] == 0
PY
if ! python3 "$root/client.py" "$root" >&3 <&4; then
	cat "$root/daemon.stderr" >&2
	cat "$root/frontend.stderr" >&2
	docker logs "$container" >&2
	exit 1
fi
exec 3>&- 4<&-
wait "$frontend_pid"
frontend_pid=""

grep -q 'worker.build_derivation.completed' "$root/daemon.stderr"
echo "local Docker alpha passed: isolated Nix daemon executed BuildDerivation through Telchar"
