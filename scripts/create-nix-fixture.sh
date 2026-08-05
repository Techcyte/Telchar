#!/bin/sh
set -eu

root=$1
state_dir="$root/state"
temp_dir="$root/tmp"
config_path="$root/nix.conf"
private_key_path="$root/client-key"

mkdir -p "$state_dir" "$temp_dir"
cat >"$config_path" <<EOF
build-users-group =
state-dir = $state_dir
temp-dir = $temp_dir
warn-dirty = false
EOF
umask 077
ssh-keygen -q -t ed25519 -N '' -f "$private_key_path"
