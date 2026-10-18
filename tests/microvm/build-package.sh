#!/usr/bin/env bash
# Local package build in a retained private store; not production sandbox qualification.
set -euo pipefail
if (($# != 2)); then
    printf 'usage: %s DERIVATION ARTIFACT_DIRECTORY\n' "$0" >&2
    exit 2
fi
derivation=$(realpath "${1%%!*}")
artifacts=$(realpath "$2")
root=$(mktemp -d "$artifacts/package-XXXXXXXX")
printf 'PACKAGE_ARTIFACTS=%s\n' "$root"
nix-store --query --requisites --include-outputs "$derivation" > "$root/paths"
mapfile -t paths < "$root/paths"
nix-store --dump-db "${paths[@]}" > "$root/registration"
mkdir "$root/upper" "$root/work" "$root/store" "$root/state" "$root/log" "$root/conf" "$root/tmp" "$root/home"
export NIX_STATE_DIR="$root/state" NIX_LOG_DIR="$root/log" NIX_CONF_DIR="$root/conf" TMPDIR="$root/tmp" HOME="$root/home"
export NIX_USER_CONF_FILES=/dev/null NIX_STORE_DIR=/nix/store NIX_REMOTE=local
export NIX_DAEMON_SOCKET_PATH="$root/unused-socket"
export NIX_CONFIG=$'sandbox = false\nbuild-users-group =\nsubstituters =\nbuilders =\nmax-jobs = 1\ncores = 1\n'
export MICROVM_PACKAGE_ROOT="$root" MICROVM_PACKAGE_DERIVATION="$derivation"
unshare --user --map-root-user --mount --fork bash -c '
set -euo pipefail
mount --make-rprivate /
mount -t overlay overlay -o "lowerdir=/nix/store,upperdir=$MICROVM_PACKAGE_ROOT/upper,workdir=$MICROVM_PACKAGE_ROOT/work" "$MICROVM_PACKAGE_ROOT/store"
mount --bind "$MICROVM_PACKAGE_ROOT/store" /nix/store
nix-store --store local --init
nix-store --store local --load-db < "$MICROVM_PACKAGE_ROOT/registration"
nix-store --store local --realise "$MICROVM_PACKAGE_DERIVATION" --option builders "" --option substitute false --max-jobs 1 --cores 1 --keep-failed > "$MICROVM_PACKAGE_ROOT/outputs"
'
