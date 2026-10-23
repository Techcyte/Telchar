#!/usr/bin/env bash
# Private canonical store with a copy-on-write view of installed executables.
set -euo pipefail
binary=$(realpath "$1")
shift
if (($# == 0)); then
    set -- --exact receipts_require_authoritative_registration --test-threads=1
fi
root=$(mktemp -d /tmp/telchar-output.XXXXXX)
cleanup() {
    status=$?
    if ((status != 0)); then
        printf 'Canonical store fixture retained: %s\n' "$root" >&2
    elif [[ -n ${MICROVM_TEST_RUNTIME:-} ]]; then
        printf 'Canonical store fixture retained: %s\n' "$root"
    else
        if [[ -d $root/client/store ]]; then
            find "$root/client/store" -type d -exec chmod u+w {} +
        fi
        rm -rf "$root"
    fi
}
trap cleanup EXIT
mkdir "$root/upper" "$root/work" "$root/store" "$root/state" "$root/log" "$root/conf" "$root/tmp"
export NIX_STATE_DIR="$root/state" NIX_LOG_DIR="$root/log" NIX_CONF_DIR="$root/conf" TMPDIR="$root/tmp"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_DAEMON_SOCKET_PATH="$root/socket"
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\ntrusted-users = root\n'
if [[ ${MICROVM_TEST_CASE:-} == configured* ]]; then
    export NIX_CONFIG+=$'extra-platforms = aarch64-linux\n'
fi
export MICROVM_STORE_ROOT="$root" MICROVM_OUTPUT_TEST="$binary" MICROVM_TEST_STORE_URI="unix://$root/socket"
unshare --user --map-root-user --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
mount -t overlay overlay -o "lowerdir=/nix/store,upperdir=$MICROVM_STORE_ROOT/upper,workdir=$MICROVM_STORE_ROOT/work" "$MICROVM_STORE_ROOT/store"
mount --bind "$MICROVM_STORE_ROOT/store" /nix/store
nix-store --store local --init
if test -n "${MICROVM_TEST_REGISTRATION:-}"; then
    nix-store --store local --load-db < "$MICROVM_TEST_REGISTRATION"
fi
nix-daemon > "$MICROVM_STORE_ROOT/daemon.log" 2>&1 & daemon=$!
trap '\''kill "$daemon"; wait "$daemon" || true'\'' EXIT
for ((i=0;i<100;i++)); do test ! -S "$NIX_DAEMON_SOCKET_PATH" || break; sleep .02; done
"$MICROVM_OUTPUT_TEST" "$@"
' bash "$@"
