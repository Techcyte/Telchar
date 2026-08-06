#!/bin/sh
set -eu

contract='docs/adr/protocol-session-limits.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

[ -f "$contract" ] || {
	printf 'missing protocol session limits contract: %s\n' "$contract" >&2
	exit 1
}

require "$contract" '16 MiB'
require "$contract" 'concurrently retained decoded metadata'
require "$contract" 'before allocation'
require "$contract" 'checked arithmetic'
require "$contract" 'Streamed payload bodies are excluded'
require "$contract" '30 seconds'
require "$contract" 'incomplete typed protocol message or frame'
require "$contract" 'resets whenever protocol input makes forward progress'
require "$contract" '`ProtocolSessionLimits`'
require "$contract" 'session-owned `WorkerReader<R>`'
require "$contract" 'one shared allocation budget for the lifetime of the protocol session'
require "$contract" 'Fixture-only slice observers that retain no protocol bodies may keep their existing fixture-bounded APIs'
require "$contract" 'Future typed operations extend `WorkerReader<R>` methods'
require "$contract" 'Telchar transport layer'
require "$contract" '`io::ErrorKind::TimedOut`'
require "$plan" '- [x] T038A Define protocol session resource limits'
require "$plan" 'Depends on: T038A'

printf 'protocol session limits contract check passed\n'
