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
mkdir "$root/upper" "$root/work" "$root/store" "$root/state" "$root/log" "$root/conf" "$root/tmp"
export NIX_STATE_DIR="$root/state" NIX_LOG_DIR="$root/log" NIX_CONF_DIR="$root/conf" TMPDIR="$root/tmp"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_DAEMON_SOCKET_PATH="$root/socket"
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\ntrusted-users = root\nmax-jobs = 0\n'
export TELCHAR_EXECUTION_ROOT="$root"
unshare --user --map-root-user --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
mount -t overlay overlay -o "lowerdir=/nix/store,upperdir=$TELCHAR_EXECUTION_ROOT/upper,workdir=$TELCHAR_EXECUTION_ROOT/work" "$TELCHAR_EXECUTION_ROOT/store"
mount --bind "$TELCHAR_EXECUTION_ROOT/store" /nix/store
"$@"
' bash "$binary" "$@"
