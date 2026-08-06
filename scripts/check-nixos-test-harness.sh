#!/bin/sh
set -eu

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

flake='flake.nix'
contract='docs/adr/nixos-test-topology.md'

require "$flake" 'nixos-smoke ='
require "$flake" 'nixos-artifacts ='
require "$contract" 'PostgreSQL, OpenSSH builder, Nomad, and cache fixtures extend this topology through the shared helpers; they do not create a second orchestration harness.'

nix eval .#checks.x86_64-linux.nixos-smoke.driver --raw >/dev/null
nix eval .#checks.x86_64-linux.nixos-artifacts.driver --raw >/dev/null

printf 'NixOS test harness gate check passed\n'
