#!/bin/sh
set -eu

policy='docs/adr/rio-source-policy.md'

require() {
	text=$1
	if ! grep -F -- "$text" "$policy" >/dev/null; then
		printf 'missing required source-policy text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$policy" ] || {
	printf 'missing source policy: %s\n' "$policy" >&2
	exit 1
}

require 'BSD 3-Clause License'
require 'MIT OR Apache-2.0'
require 'must not copy, translate, or mechanically adapt'
require 'Reference evidence'
require 'Implementation evidence'
require 'separate explicit import decision'

printf 'rio source policy review checklist passed\n'
