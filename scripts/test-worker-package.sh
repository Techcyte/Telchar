#!/usr/bin/env bash
# Supply worker test prerequisites in a private root without changing the build sandbox.
set -euo pipefail
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
        /var|/noshell) continue ;;
    esac
    if [[ -L $path ]]; then
        mounts+=(--symlink "$(readlink "$path")" "$path")
    elif [[ $path == /dev ]]; then
        mounts+=(--dev-bind "$path" "$path")
    else
        mounts+=(--bind "$path" "$path")
    fi
done
bwrap --unshare-user --uid 1000 --gid 1000 --die-with-parent \
    --tmpfs / "${mounts[@]}" --dir /var/empty \
    --symlink "$(command -v bash)" /noshell \
    --ro-bind "$root/passwd" /etc/passwd \
    --ro-bind "$root/group" /etc/group \
    --chdir "$PWD" -- "$@"
