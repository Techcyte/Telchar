#!/bin/sh
set -eu

adr='docs/adr/singleton-ownership.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'

require() {
	text=$1
	if ! grep -F -- "$text" "$adr" >/dev/null; then
		printf 'missing singleton ownership contract text: %s\n' "$text" >&2
		exit 1
	fi
}

[ -f "$adr" ] || {
	printf 'missing ADR: %s\n' "$adr" >&2
	exit 1
}

require '0x5445_4c43_4841_5202'
require 'pg_try_advisory_lock'
require 'dedicated lifetime PostgreSQL connection'
require 'before admission, scheduling, reconciliation, administrative mutation, backend submission, or listener readiness'
require 'Contention'
require 'Database disconnect'
require 'Reconnect'
require 'Graceful shutdown'
require 'Process crash'
require 'No reconnect preserves or restores ownership.'
require 'does not provide high availability'
require 'database.singleton_ownership.acquired'
require 'database.singleton_ownership.refused'
require 'database.singleton_ownership.lost'

if ! grep -F -- '- [x] T101A Define PostgreSQL singleton ownership contract' "$plan" >/dev/null; then
	printf 'T101A is not complete in master plan\n' >&2
	exit 1
fi

printf 'singleton ownership contract check passed\n'
