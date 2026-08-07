#!/bin/sh
set -eu

adr=docs/adr/gateway-store-ownership.md
[ -f "$adr" ] || { printf 'missing gateway-store ownership ADR: %s\n' "$adr" >&2; exit 1; }

require() {
    grep -F -- "$1" "$adr" >/dev/null || {
        printf 'missing gateway-store ownership requirement: %s\n' "$1" >&2
        exit 1
    }
}

for text in \
    '## Scope and trust boundary' \
    '## Service account and process ownership' \
    '## Daemon interaction' \
    '## Required privileges' \
    '## Garbage collection ownership' \
    '## Excluded workloads' \
    'single-active fence' \
    'Only the Telchar daemon may initiate or schedule gateway-store garbage collection.' \
    'must not receive host package management' \
    '[x] Dedicated gateway-store owner and store root are named.' \
    '[x] Required daemon privileges and forbidden privileges are listed.'; do
    require "$text"
done

printf 'gateway-store ownership checklist passed\n'
