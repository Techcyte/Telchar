#!/usr/bin/env bash
# Runs execution contracts with a private canonical Nix store and daemon.
set -euo pipefail
binary=$(realpath "$1")
shift
root=$(mktemp -d /tmp/telchar-execution.XXXXXX)
cleanup() {
    status=$?
    if ((status != 0)); then
        printf 'Execution fixture retained: %s\n' "$root" >&2
    else
        find "$root" -type d -exec chmod u+w {} +
        rm -rf "$root"
    fi
}
trap cleanup EXIT
mkdir "$root/store" "$root/state" "$root/log" "$root/conf" "$root/tmp"
export NIX_STATE_DIR="$root/state" NIX_LOG_DIR="$root/log" NIX_CONF_DIR="$root/conf" TMPDIR="$root/tmp"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_DAEMON_SOCKET_PATH="$root/socket"
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\ntrusted-users = root\nmax-jobs = 0\n'
export TELCHAR_EXECUTION_ROOT="$root"
unshare --user --map-root-user --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
roots=()
IFS=: read -ra directories <<< "$PATH"
for path in "${directories[@]}" /bin/sh; do
    [[ -e $path ]] || continue
    path=$(realpath "$path")
    if [[ $path == /nix/store/* ]]; then
        path=${path#/nix/store/}
        roots+=("/nix/store/${path%%/*}")
    fi
done
ldd "$1" | while read -r line; do
    for path in $line; do
        if [[ $path == /nix/store/* ]]; then
            path=${path#/nix/store/}
            printf "/nix/store/%s\n" "${path%%/*}"
        fi
    done
done > "$TELCHAR_EXECUTION_ROOT/libraries"
mapfile -t libraries < "$TELCHAR_EXECUTION_ROOT/libraries"
nix-store --store unix:///nix/var/nix/daemon-socket/socket --query --requisites "${roots[@]}" "${libraries[@]}" | sort -u > "$TELCHAR_EXECUTION_ROOT/closure"
while IFS= read -r path; do
    target="$TELCHAR_EXECUTION_ROOT/store/${path##*/}"
    if [[ -d $path ]]; then
        mkdir "$target"
        mount --rbind "$path" "$target"
    else
        cp -a --no-preserve=ownership "$path" "$target"
    fi
done < "$TELCHAR_EXECUTION_ROOT/closure"
mount --rbind "$TELCHAR_EXECUTION_ROOT/store" /nix/store
"$@"
' bash "$binary" "$@"
