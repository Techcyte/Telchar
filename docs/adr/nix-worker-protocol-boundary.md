# Nix worker protocol crate boundary

**Status:** Accepted

## Decision

`nix-worker-protocol` owns reusable, versioned Nix worker-wire behavior only. Its permitted responsibilities are:

- wire primitives;
- negotiation;
- operations;
- messages;
- activity/error frames;
- result types;
- compatibility fixtures;
- property tests; and
- fuzz targets.

It may expose bounded types and streaming interfaces needed to parse, encode, classify, and transparently relay the supported protocol. The crate may emit `tracing` instrumentation, but it must not own OpenTelemetry exporters.

The crate must not depend on or contain Telchar domain policy for identity, scheduler, PostgreSQL, SSH ingress, backend, cache, or service configuration. Telchar service code owns those concerns and may depend on `nix-worker-protocol`, never the reverse.

## Enforcement

`sh scripts/check-protocol-boundary.sh` rejects direct protocol-crate dependencies on Telchar and established domain or service dependency names. Dependency additions require extending this decision and check before use.

## Consequences

Wire behavior remains reusable independently of Telchar deployment decisions. Protocol-level tests can use compatibility fixtures, property tests, and fuzz targets without importing services or infrastructure.
