# Telchar MVP operator checklist

## Deployment boundary

- [ ] Deploy Telchar as a trusted Nix build gateway, not as a general CI scheduler.
- [ ] Expose stock Nix ingress only through the restricted OpenSSH account and forced command.
- [ ] Keep the daemon IPC socket private to the configured frontend UID.
- [ ] Give the Telchar daemon access only to its configured PostgreSQL database, gateway store, state directories, and backend credentials.
- [ ] Terminate public callback TLS at an operator-managed reverse proxy or load balancer; Telchar's callback listener speaks plaintext WebSocket.
- [ ] Forward WebSocket upgrade requests and the exact `telchar-nomad-transfer-v1` subprotocol unchanged.
- [ ] Restrict the plaintext proxy-to-Telchar hop to a trusted network or local interface.
- [ ] Set callback proxy idle timeout above Telchar's transfer idle timeout.
- [ ] Disable automatic request retries at callback proxies. One WebSocket remains bound to one transfer; stickiness is unnecessary.

## Credentials and identities

- [ ] Keep PostgreSQL passwords, SSH identities, Nomad tokens, HMAC keys, and cache credentials outside the Nix store. Keep TLS keys in the terminating proxy or load balancer, never Telchar configuration.
- [ ] Use `services.telchar.credentials` or another protected deployment mechanism and refer to files below `CREDENTIALS_DIRECTORY` from Telchar TOML.
- [ ] Pin static SSH host keys. Do not use permissive host-key checking.
- [ ] Configure workload identity with explicit issuer, JWKS URL, audience, and optional CA file. Do not infer trust from the Nomad API endpoint.
- [ ] Treat workload JWTs and HMAC capabilities as bearer credentials. Authentication is mandatory even on plaintext trusted networks.
- [ ] Remember that HMAC over plaintext provides authentication and integrity, not confidentiality.

## Gateway and allocation stores

- [ ] Run the gateway store as the primary output and restart-recovery authority.
- [ ] Grant the Telchar daemon the minimum Nix daemon trust needed for closure queries, NAR export/import, builds, and retention roots.
- [ ] Place the configured GC-root directory on persistent storage and monitor its capacity.
- [ ] Decide whether Nomad workers use a host Nix daemon, allocation-local daemon, or local store.
- [ ] Treat host daemon socket mounts as privileged Nomad operator policy. Telchar never injects arbitrary mounts.
- [ ] Configure allocation substituters, public keys, and credentials independently of client requests.

## PostgreSQL coordination

- [ ] Back up PostgreSQL according to the desired recovery point.
- [ ] Allow only one active Telchar scheduling owner for a database.
- [ ] Monitor migration and singleton-ownership failures as startup failures.
- [ ] Preserve `shared_builds`, attempts, admitted build specifications, transfer phases, and terminal metadata across daemon restart.
- [ ] Never place NAR bodies, capabilities, signatures, raw nonces, credentials, or logs in PostgreSQL.

## Backend targets

- [ ] Give every local, static SSH, and Nomad target a unique operator-controlled name.
- [ ] Configure each target's systems, features, capacity, credentials, and execution policy explicitly.
- [ ] Do not expose backend names, clusters, stores, credentials, drivers, cache policy, or permits to stock Nix client selection.
- [ ] Understand that compatible targets are fungible only before routing. In-flight execution remains bound to its original target.

## Restart recovery

- [ ] Expect Telchar to trust exact valid gateway-store outputs first.
- [ ] For static SSH, retain the exact backend configuration and import every expected output from that backend.
- [ ] For Nomad, retain the exact cluster endpoint, namespace, backend name, and persisted execution ID.
- [ ] Treat missing, malformed, foreign, timed-out, mismatched, or unverifiable recovery as terminal failure.
- [ ] Do not expect blind resubmission or migration. Telchar performs no automatic retry.

## Client behavior

- [ ] Supported gateway protocol range is client maximum 1.38, daemon minimum 1.35, required major 1.
- [ ] Only normal build mode `0` is accepted. Repair and check modes are rejected.
- [ ] Fixed-output derivations remain post-MVP and are rejected.
- [ ] A requester disconnect does not cancel admitted execution under detach-and-finish policy.
- [ ] A later independent stock Nix request may replace a failed shared-build row according to normal Nix retry behavior; Telchar itself does not retry the failed attempt.
- [ ] Followers share one execution and consume no additional backend permit or execution quota.

## Logs and observability

- [ ] Treat build logs as bounded and connection-scoped.
- [ ] Do not promise replay to late followers or after reconnect/restart.
- [ ] Export operational telemetry and systemd journals to operator-owned observability systems.
- [ ] Use external bounded archival tooling when historical build logs are required.

## Shutdown and limits

- [ ] Configure authentication, transfer idle, output collection, maximum connection lifetime, setup, runtime, polling, follower, and callback drain bounds independently.
- [ ] Allow callback shutdown to stop acceptance, drain active transfers, close survivors, and join callback threads.
- [ ] Size maximum connections, manifests, NARs, frames, queues, diagnostics, and retained nonces for available memory and disk.
- [ ] Monitor gateway disk reserve and temporary NAR spool capacity.

## Explicit non-goals

Telchar MVP does not provide:

- generic CI pipelines or provider provisioning;
- priorities, billing, or active/active scheduling;
- automatic build retries or migration of in-flight work;
- client-selected backend identity or credentials;
- a binary cache, cache credential broker, or publication service;
- PostgreSQL log storage or historical log replay;
- Redis or object-storage log clients;
- fixed-output derivation execution;
- arbitrary Nomad host mounts or task interpolation from client bytes;
- native TLS termination or certificate lifecycle management.

## Release checks

- [ ] Build `.#telchar` and `.#telchar-nomad-worker`.
- [ ] Run `.#checks.x86_64-linux.nixos-module`.
- [ ] Run local, static SSH, and Nomad stock-Nix gateway fixtures.
- [ ] Run duplicate coalescing and restart-reconciliation fixtures.
- [ ] Run formatting, tests, check, and clippy with warnings denied.
- [ ] Record exact Nix, Nomad, PostgreSQL, OpenSSH, Rust, and Telchar revisions used by the release candidate.
