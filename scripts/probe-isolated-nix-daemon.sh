#!/bin/sh
set -eu

root=$(mktemp -d "${TMPDIR:-/tmp}/telchar-nix-daemon-probe.XXXXXX")
trap 'rm -rf "$root"' EXIT INT TERM

store_dir="$root/store"
state_dir="$root/state"
log_dir="$root/log"
config_dir="$root/config"
socket_dir="$root/socket"
socket_path="$socket_dir/daemon.sock"
trusted_config="$root/trusted.conf"
untrusted_config="$root/untrusted.conf"
trusted_log="$root/trusted.log"
untrusted_log="$root/untrusted.log"

mkdir -p "$store_dir" "$state_dir" "$log_dir" "$config_dir" "$socket_dir"

write_config() {
	trusted_users=$1
	config=$2
	cat >"$config" <<EOF
build-users-group =
allowed-users = *
trusted-users = $trusted_users
sandbox = false
EOF
}

run_case() {
	name=$1
	config=$2
	log=$3
	NIX_STORE_DIR="$store_dir" \
		NIX_STATE_DIR="$state_dir" \
		NIX_LOG_DIR="$log_dir" \
		NIX_CONF_DIR="$config_dir" \
		NIX_DAEMON_SOCKET_PATH="$socket_path" \
		NIX_CONFIG="$(cat "$config")" \
		nix-daemon >"$log" 2>&1 &
	daemon_pid=$!
	for _ in $(seq 1 100); do
		[ -S "$socket_path" ] && break
		sleep 0.05
	done
	if [ ! -S "$socket_path" ]; then
		wait "$daemon_pid" || true
		printf '%s daemon did not create fixture socket\n' "$name" >&2
		sed -n '1,80p' "$log" >&2
		exit 1
	fi
	NIX_STORE_DIR="$store_dir" \
		NIX_STATE_DIR="$state_dir" \
		NIX_LOG_DIR="$log_dir" \
		NIX_CONF_DIR="$config_dir" \
		NIX_DAEMON_SOCKET_PATH="$socket_path" \
		NIX_CONFIG="$(cat "$config")" \
		nix --store "unix://$socket_path" store info --json >"$root/$name.json"
	kill "$daemon_pid"
	wait "$daemon_pid" || true
	rm -f "$socket_path"
}

write_config "$(id -un)" "$trusted_config"
run_case trusted "$trusted_config" "$trusted_log"
write_config root "$untrusted_config"
run_case untrusted "$untrusted_config" "$untrusted_log"

printf 'trusted: '
cat "$root/trusted.json"
printf '\nuntrusted: '
cat "$root/untrusted.json"
printf '\nfixture root: %s\n' "$root"
