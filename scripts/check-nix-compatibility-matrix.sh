#!/bin/sh
set -eu

matrix='docs/nix-compatibility-matrix.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

[ -f "$matrix" ] || {
	printf 'missing compatibility matrix: %s\n' "$matrix" >&2
	exit 1
}

require "$matrix" 'Stock Nix 2.34.7'
require "$matrix" '04607e1165ac22c5fde6dcc54c9e0b3c0487c555'
require "$matrix" '1.18 through 1.38'
require "$matrix" 'Lix | Deferred'
require "$matrix" 'Trusted | Classic input-addressed'
require "$matrix" 'Untrusted | Classic input-addressed'
require "$matrix" 'Content-addressed'
require "$matrix" 'trusted-classic-build-v1'
require "$matrix" 'Trusted trace accepted'
require "$matrix" 'Pending T014 trace'
require "$matrix" 'Pending T015 resolution'
require "$plan" '- [x] T010 Record initial Nix compatibility matrix'

printf 'Nix compatibility matrix validation passed\n'
