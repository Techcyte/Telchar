#!/bin/sh
set -eu

root=$1
state_dir="$root/state"
store_dir="$root/store"
log_dir="$root/log"
config_dir="$root/config"
socket_dir="$root/socket"
temp_dir="$root/tmp"
config_path="$root/nix.conf"
private_key_path="$root/client-key"

mkdir -p "$state_dir" "$store_dir" "$log_dir" "$config_dir" "$socket_dir" "$temp_dir"
cat >"$config_path" <<EOF
build-users-group =
experimental-features = nix-command
state-dir = $state_dir
temp-dir = $temp_dir
warn-dirty = false
EOF
umask 077
ssh-keygen -q -t ed25519 -N '' -f "$private_key_path"
