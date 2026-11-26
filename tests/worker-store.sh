#!/usr/bin/env bash
# Private canonical store preserving installed executable mount trees.
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
    elif [[ -n ${TELCHAR_WORKER_TEST_RUNTIME:-} ]]; then
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
if [[ ${TELCHAR_WORKER_TEST_CASE:-} == supervised ]]; then
    builder=$(realpath "$(command -v bash)")
    packages=("$(dirname "$(dirname "$builder")")")
    if [[ -n ${TELCHAR_WORKER_BENCH_MODE:-} ]]; then
        # Benchmark builders hash the full shared input with the real Nix coreutils.
        packages+=("$(dirname "$(dirname "$(command -v sha256sum)")")")
    fi
    source_store=${TELCHAR_TEST_SOURCE_STORE:-auto}
    nix-store --extra-experimental-features read-only-local-store --store "$source_store" --query --requisites "${packages[@]}" > "$root/builder-paths"
    mapfile -t builder_paths < "$root/builder-paths"
    nix-store --extra-experimental-features read-only-local-store --store "$source_store" --dump-db "${builder_paths[@]}" > "$root/builder-registration"
    export TELCHAR_WORKER_TEST_REGISTRATION="$root/builder-registration"
fi
export NIX_STATE_DIR="$root/state" NIX_LOG_DIR="$root/log" NIX_CONF_DIR="$root/conf" TMPDIR="$root/tmp"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_DAEMON_SOCKET_PATH="$root/socket"
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\ntrusted-users = root\n'
# Connected fixtures use synthetic x86 and ARM derivations with shell-only
# builders. Permit both on native ARM CI as well as native x86 CI.
export NIX_CONFIG+=$'extra-platforms = aarch64-linux x86_64-linux\n'
export TELCHAR_WORKER_STORE_ROOT="$root" TELCHAR_WORKER_TEST_BINARY="$binary" TELCHAR_WORKER_TEST_STORE_URI="unix://$root/socket"
mapping=()
# The package wrapper already provides private passwd/group entries. Its
# single-ID namespace cannot delegate additional subordinate IDs.
if [[ ${TELCHAR_WORKER_TEST_CASE:-} == supervised && $(id -u) != 0 && ${TELCHAR_WORKER_PACKAGE_TEST:-} != 1 ]]; then
    mapping+=(--user --map-auto --map-root-user)
else
    mapping+=(--user --map-root-user)
fi
if [[ ${TELCHAR_WORKER_TEST_CASE:-} == supervised && $(id -u) == 0 ]]; then
    mapping=()
fi
unshare "${mapping[@]}" --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
mount -t overlay overlay -o "userxattr,lowerdir=/nix/store,upperdir=$TELCHAR_WORKER_STORE_ROOT/upper,workdir=$TELCHAR_WORKER_STORE_ROOT/work" "$TELCHAR_WORKER_STORE_ROOT/store"
mount --bind "$TELCHAR_WORKER_STORE_ROOT/store" /nix/store
nix-store --store local --init
if test -n "${TELCHAR_WORKER_TEST_REGISTRATION:-}"; then
    nix-store --store local --load-db < "$TELCHAR_WORKER_TEST_REGISTRATION"
fi
nix-daemon > "$TELCHAR_WORKER_STORE_ROOT/daemon.log" 2>&1 & daemon=$!
trap '\''kill "$daemon"; wait "$daemon" || true'\'' EXIT
for ((i=0;i<100;i++)); do test ! -S "$NIX_DAEMON_SOCKET_PATH" || break; sleep .02; done
"$TELCHAR_WORKER_TEST_BINARY" "$@"
' bash "$@"
