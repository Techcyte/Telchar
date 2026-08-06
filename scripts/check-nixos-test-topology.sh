#!/bin/sh
set -eu

contract='docs/adr/nixos-test-topology.md'
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
	printf 'missing NixOS test topology contract: %s\n' "$contract" >&2
	exit 1
}

require "$contract" 'The authoritative whole-system acceptance harness is a flake-exported multi-machine `nixosTest`.'
require "$contract" 'stock-client'
require "$contract" 'gateway'
require "$contract" 'otlp-collector'
require "$contract" 'The virtual-network topology is asserted independently from Telchar protocol reachability.'
require "$contract" 'The packaged `telchar` systemd oneshot service owns startup.'
require "$contract" 'Baseline readiness requires the packaged Telchar systemd oneshot service to complete successfully and correlated OTLP startup telemetry to reach the collector.'
require "$contract" 'Real client-to-Telchar protocol reachability remains assigned to Gate 2 OpenSSH integration tasks.'
require "$contract" 'Failure capture retains bounded, redacted service journals, machine state, OTLP records, and test-driver output.'
require "$contract" 'Secrets enter only through NixOS test secret facilities or runtime files with mode `0600`.'
require "$contract" 'PostgreSQL, OpenSSH builder, Nomad, and cache fixtures extend this topology through the shared helpers; they do not create a second orchestration harness.'
require "$contract" 'T021C'
require "$contract" 'T021D'
require "$contract" 'T021E'
require "$plan" '- [x] T021A Define reusable `nixosTest` topology contract'

printf 'NixOS test topology contract check passed\n'
