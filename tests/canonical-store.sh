#!/usr/bin/env bash
# Run a command with a disposable canonical store and a nonroot Nix daemon.
set -euo pipefail
if (($# == 0)); then
    printf 'Usage: bash tests/canonical-store.sh COMMAND [ARG ...]\n' >&2
    exit 2
fi
TELCHAR_STORE_UID=$(id -u)
TELCHAR_STORE_GID=$(id -g)
TELCHAR_STORE_USER=$(id -un)
export TELCHAR_STORE_UID TELCHAR_STORE_GID TELCHAR_STORE_USER
if ((TELCHAR_STORE_UID == 0)); then
    printf 'Canonical store command requires a nonroot caller for PostgreSQL fixtures\n' >&2
    exit 2
fi
export TELCHAR_STORE_ROOT
TELCHAR_STORE_ROOT=$(mktemp -d /tmp/telchar-store.XXXXXX)
printf 'Canonical store fixture retained: %s\n' "$TELCHAR_STORE_ROOT"
mkdir "$TELCHAR_STORE_ROOT"/{upper,work,store,state,log,conf,tmp,socket-dir}
export NIX_STATE_DIR="$TELCHAR_STORE_ROOT/state" NIX_LOG_DIR="$TELCHAR_STORE_ROOT/log"
export NIX_CONF_DIR="$TELCHAR_STORE_ROOT/conf" TMPDIR="$TELCHAR_STORE_ROOT/tmp"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_REMOTE=local
export NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\nbuilders =\nmax-jobs = 1\ncores = 1\n'
export NIX_CONFIG+="trusted-users = $TELCHAR_STORE_USER"
unshare --user --map-root-user --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
mount -t overlay overlay -o "lowerdir=/nix/store,upperdir=$TELCHAR_STORE_ROOT/upper,workdir=$TELCHAR_STORE_ROOT/work" "$TELCHAR_STORE_ROOT/store"
mount --bind "$TELCHAR_STORE_ROOT/store" /nix/store
mount --bind "$TELCHAR_STORE_ROOT/socket-dir" /nix/var/nix/daemon-socket
exec unshare --user --map-user="$TELCHAR_STORE_UID" --map-group="$TELCHAR_STORE_GID" bash -c '\''
set -euo pipefail
[[ $(id -u) == "$TELCHAR_STORE_UID" && $(id -un) == "$TELCHAR_STORE_USER" ]]
printf "DISPOSABLE_STORE_TRUST=%s uid=%s\n" "$TELCHAR_STORE_USER" "$(id -u)"
nix-store --store local --init
nix-daemon > "$TELCHAR_STORE_ROOT/daemon.log" 2>&1 & daemon=$!
cleanup() {
    kill "$daemon"
    wait "$daemon" || true
}
trap cleanup EXIT
for ((i=0;i<100;i++)); do
    [[ ! -S "$NIX_DAEMON_SOCKET_PATH" ]] || break
    kill -0 "$daemon"
    sleep .02
done
[[ -S "$NIX_DAEMON_SOCKET_PATH" ]]
"$@"
'\'' bash "$@"
' bash "$@"
