#!/usr/bin/env bash
# Supply worker test prerequisites in a private root without changing the build sandbox.
set -euo pipefail
workerTestRegistration=${workerTestRegistration:?private closure registration is required}
store=$(realpath "$1")
shift
root=$(mktemp -d)
cleanup() {
    rm -rf "$root"
}
trap cleanup EXIT
printf 'root:x:0:0:root:/root:/noshell\nrunner::1000:1000:Worker tests:/tmp:/noshell\nnobody:x:65534:65534:nobody:/var/empty:/noshell\n' > "$root/passwd"
printf 'root:x:0:\nrunner:x:1000:\nnobody:x:65534:\n' > "$root/group"
mounts=()
for path in /*; do
    case "$path" in
        /var|/nix|/bin|/noshell) continue ;;
    esac
    if [[ -L $path ]]; then
        mounts+=(--symlink "$(readlink "$path")" "$path")
    elif [[ $path == /dev ]]; then
        mounts+=(--dev-bind "$path" "$path")
    else
        mounts+=(--bind "$path" "$path")
    fi
done
# Supervised MicroVM fixtures need mount authority in this private namespace;
# individual unprivileged-client fixtures create their own nonroot namespaces.
bwrap --unshare-user --uid 0 --gid 0 --die-with-parent \
    --tmpfs / "${mounts[@]}" --tmpfs /tmp --dir /var/empty \
    --dir /nix/var/nix/profiles \
    --symlink "$(command -v bash)" /noshell \
    --symlink "$(command -v bash)" /bin/sh \
    --ro-bind "$root/passwd" /etc/passwd \
    --ro-bind "$root/group" /etc/group \
    --bind "$store" /nix/store \
    --ro-bind "$workerTestRegistration" /worker-registration \
    --chdir "$PWD" -- bash -c '
        set -euo pipefail
        nix-store --store local --init
        nix-store --store local --load-db < /worker-registration
        exec "$@"
    ' bash "$@"
