#!/bin/sh
set -eu

inventory='docs/rio-test-category-inventory.md'

[ -f "$inventory" ] || {
    printf 'missing Rio test-category inventory: %s\n' "$inventory" >&2
    exit 1
}

for text in \
    '## Scope and source boundary' \
    '## Reference-to-test-category checklist' \
    '## Review result' \
    'Adopted' \
    'Deferred' \
    'Rejected' \
    '59e832144d67c1b1973272ef394ffc6ef2629f4b' \
    'copy, translate, or mechanically adapt Rio implementation code or test bodies.' \
    'T044' \
    'T045' \
    'T258'
do
    grep -F -- "$text" "$inventory" >/dev/null || {
        printf 'missing required Rio test-category inventory text: %s\n' "$text" >&2
        exit 1
    }
done

printf 'Rio test-category inventory checklist passed\n'
