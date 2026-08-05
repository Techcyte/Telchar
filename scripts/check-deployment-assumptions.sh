#!/bin/sh
set -eu

adr='docs/adr/deployment-assumptions.md'
design='telchar-design.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'

require() {
	file=$1
	text=$2
	if ! grep -F -- "$text" "$file" >/dev/null; then
		printf 'missing required text in %s: %s\n' "$file" "$text" >&2
		exit 1
	fi
}

[ -f "$adr" ] || {
	printf 'missing ADR: %s\n' "$adr" >&2
	exit 1
}

require "$adr" 'Linux-first initial support.'
require "$adr" 'single-active deployment'
require "$adr" 'OpenSSH provides network-facing SSH ingress.'
require "$adr" 'restricted forced command starts one `telchar serve-stdio` frontend per connection.'
require "$adr" 'authenticated local IPC'
require "$adr" 'PostgreSQL is the durable control-plane database.'
require "$adr" 'does not provide multiple active schedulers or Telchar high availability.'
require "$adr" 'domain-specific state operations with explicit transaction ownership.'
require "$adr" 'Database interchangeability is not an initial goal.'
require "$adr" 'TOML is the initial human-readable service configuration format.'
require "$adr" 'dedicated host or VM whose system Nix store is controlled by Telchar and is not shared with unrelated workloads.'
require "$adr" 'one mutually trusted store domain'
require "$adr" 'Hostile client multi-tenancy and per-path client authorization are deferred.'

require "$design" 'The first release is single-active: one Telchar daemon owns scheduling, durable state, gateway-store coordination, and backend reconciliation.'
require "$design" 'The initial topology uses OpenSSH as the network-facing SSH implementation.'
require "$design" 'A restricted forced command starts one `telchar serve-stdio` frontend per connection.'
require "$design" 'it communicates over authenticated local IPC with the single Telchar daemon.'
require "$design" 'The first implementation uses PostgreSQL as its durable control-plane database while retaining an explicit single-active Telchar daemon constraint.'
require "$design" 'PostgreSQL is an infrastructure choice, not a claim of scheduler high availability.'
require "$design" 'Database interchangeability is not an initial goal.'
require "$design" 'The first deployment uses a dedicated gateway host or VM whose system Nix store is controlled by Telchar and not shared with unrelated host workloads.'
require "$design" 'All authenticated clients in the first release belong to one mutually trusted store domain.'
require "$plan" '- [x] T001 Record initial supported deployment assumptions'

if grep -Ein 'sqlite.*(durable|control-plane|database)|(durable|control-plane|database).*sqlite' "$adr" "$design" | grep -Eiv 'not.*sqlite|sqlite.*(not|rather than|substitutes)' >/dev/null; then
	printf 'SQLite is described as a selected database\n' >&2
	exit 1
fi

if grep -Ein 'postgresql.*(automatically|provides|enables).*(high availability|multiple active schedulers)|(high availability|multiple active schedulers).*(automatically|provided|enabled).*postgresql' "$adr" "$design" >/dev/null; then
	printf 'PostgreSQL is described as automatically providing scheduler high availability\n' >&2
	exit 1
fi

printf 'deployment documentation consistency check passed\n'
