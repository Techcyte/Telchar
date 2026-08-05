#!/bin/sh
set -eu

reference='docs/rio-build-reference.md'
revision='59e832144d67c1b1973272ef394ffc6ef2629f4b'
url='https://github.com/lovesegfault/rio-build'

require() {
	text=$1
	if ! grep -F -- "$text" "$reference" >/dev/null; then
		printf 'missing required reference text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$reference" ] || {
	printf 'missing reference record: %s\n' "$reference" >&2
	exit 1
}

require "$url"
require "$revision"
require 'Architecture and test-category research only'

printf 'rio-build reference provenance validation passed\n'
