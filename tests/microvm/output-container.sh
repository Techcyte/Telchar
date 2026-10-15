#!/usr/bin/env bash
# Isolated production-namespace store; no host store mounted writable.
set -euo pipefail
binary=$(realpath "$1")
image=telchar-microvm-probe:experimental
container=$(docker run --detach --privileged --network none "$image")
trap 'docker rm -f "$container" >/dev/null' EXIT
mapfile -t libraries < <(ldd "$binary" | awk '/=> \/nix\/store/ {print $3} /^\s*\/nix\/store/ {print $1}')
for library in "${libraries[@]}"; do
    docker exec "$container" /bin/mkdir -p "$(dirname "$library")"
    docker cp -L "$library" "$container:$library" >/dev/null
done
# Include the ELF interpreter even when ldd prefixes its line with tabs.
interpreter=$(patchelf --print-interpreter "$binary")
docker exec "$container" /bin/mkdir -p "$(dirname "$interpreter")"
docker cp -L "$interpreter" "$container:$interpreter" >/dev/null
docker cp "$binary" "$container:/run/output-test" >/dev/null
for _ in $(seq 1 100); do
    if docker exec "$container" /bin/nix --offline store ping --store daemon >/dev/null 2>&1; then
        break
    fi
    sleep .1
done
docker exec -e MICROVM_TEST_STORE_URI=unix:///nix/var/nix/daemon-socket/socket "$container" \
    /run/output-test --exact tests::returned_nar_is_validated_and_registered_in_real_store --test-threads=1
logs=$(docker logs "$container" 2>&1)
printf '%s\n' "$logs"
if printf '%s\n' "$logs" | grep -Ei 'error:|warning:'; then
    exit 1
fi
