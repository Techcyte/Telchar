# Telchar MVP release verification

Verification date: 2026-08-13

## Candidate

```text
Telchar revision: c4e5c91442f435d3970c5ba12284389abe06f767
Nix:              2.34.7
Nomad:            1.11.3
PostgreSQL:       17.10
OpenSSH:          10.4p1, OpenSSL 3.6.3
Rust:             1.95.0
Schema:           15
```

## Authoritative command

```sh
./scripts/check-release.sh
```

Result: pass.

The command verifies:

- rustfmt with no residue;
- full locked workspace tests;
- full locked workspace type checking;
- all-target clippy with warnings denied;
- reproducible `telchar` and `telchar-nomad-worker` packages;
- opinionated public NixOS module boot and service ownership;
- local stock-Nix Gate 3 build contract;
- static SSH routing, coalescing, and exact output import;
- Nomad worker input transfer, exact build, output return, requester disconnect, follower attachment, durable completion, and completed-output reuse;
- exact shared-build restart recovery for gateway outputs, static SSH, and adoptable executions.

## Demonstrated MVP contract

```text
stock Nix ssh-ng client
→ restricted Telchar ingress
→ validated BuildDerivation
→ one durable shared execution
→ compatible operator-configured backend
→ client-independent execution
→ exact validated gateway outputs
→ normal Nix BuildResult
```

Backends included in the verified contract:

- local executor;
- static SSH targets;
- Nomad targets using the packaged allocation worker.

Recovery remains exact-target and exact-execution bound. Telchar does not blindly resubmit or migrate in-flight work.

## Residual limitations

- Fixed-output derivations are rejected and remain post-MVP.
- Logs are bounded and connection-scoped; PostgreSQL stores no build log bytes and replay is not provided.
- Telchar performs no automatic build retry.
- Cache publication and log archival require external operator tooling.
- Native TLS termination is an explicit non-goal; public WSS endpoints use an operator-managed reverse proxy or load balancer.
- Workload identity or HMAC authentication remains mandatory on plaintext trusted networks.
- Retention-maintenance and recovery-monitor background threads are owned lifecycle services under completed roadmap task T147.
- The historical `nixos-restart-reconciliation` fixture targets schema authority removed by migration 13. Release verification uses the current eight-case `shared_build_recovery` suite instead.

## Release conclusion

The candidate satisfies the MVP gateway contract: one durable shared execution per equivalent derivation, compatible-backend fan-out before execution, client-independent monitoring, exact output validation/import, restart-safe backend identity, strict operator-controlled configuration, reproducible packages, and an opinionated NixOS service module.
