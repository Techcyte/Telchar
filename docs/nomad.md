# Nomad backend

The Nomad backend submits one deterministic batch job for each admitted shared build. A packaged `telchar-nomad-worker` runs inside the allocation and opens the data connection back to Telchar.

```text
Telchar submits job
  → optional prestart task
  → worker authenticates to callback
  → worker resolves admitted inputs
  → BuildDerivation through allocation-side Nix
  → live logs and exact outputs return to Telchar
```

Nomad placement and autoscaling are external concerns. Allocation state `complete` is not build success; every declared output must be validated, imported, and confirmed in the gateway store.

## API and callback endpoints

The Nomad API endpoint and transfer endpoint are separate settings:

- Nomad API: `http://` or `https://`;
- transfer endpoint: `ws://` or externally terminated `wss://`;
- required WebSocket subprotocol: `telchar-nomad-transfer-v1`.

Telchar's callback listener is plaintext WebSocket. Public `wss://` requires an operator-managed reverse proxy or load balancer. The proxy must preserve upgrades and the subprotocol, disable retries, and keep its idle timeout above Telchar's transfer idle timeout.

## Authentication

Every transfer is authenticated, including on trusted plaintext networks.

### Workload identity

Configure an explicit issuer, JWKS URL, audience, and optional CA certificate. Telchar verifies the signature and exact namespace, job, allocation, and task claims. It does not infer identity trust from the Nomad API endpoint.

### HMAC capability

Telchar can sign a short-lived capability with a protected backend key. The allocation receives the scoped capability, not the backend signing key. Replay, expiry, backend, namespace, job, request digest, and callback binding are checked before transfer.

HMAC provides authentication and integrity. On `ws://`, it does not hide tokens, paths, derivation metadata, logs, or NAR data from the network.

## Allocation-side Nix store

The worker uses the configured Nix store or daemon. Common choices are:

- a mounted host Nix daemon with a persistent warm store;
- an allocation-local daemon or store with ordinary substituters.

Mounting a host daemon socket is privileged operator policy. Telchar does not create arbitrary mounts or allow clients to select the store.

For each path in the complete admitted closure manifest, the worker:

1. checks the allocation store;
2. lets its configured substituters resolve missing paths;
3. checks validity again;
4. requests only unresolved admitted paths from Telchar.

The manifest is transfer authority. Cache availability changes traffic volume, not which paths may be requested. NARs are streamed in ordered, non-interleaved chunks with exact offsets and declared sizes.

## Optional prestart task

A backend may add one Nomad lifecycle `prestart` task in the same job and task group. It has operator-controlled driver configuration, bounded resources, and a finite timeout. Typical uses include preparing `nix.conf`, cache credentials, proxies, mounts, or allocation directories.

Client data is never interpolated into the prestart command or driver configuration. Failure prevents the build task from starting and terminates the attempt without retry.

## Logs and outputs

Logs are bounded and delivered only to clients attached at the time. Slow or disconnected clients cannot block the worker. Telchar does not store log bytes in PostgreSQL or replay them after reconnect.

After `BuildDerivation` succeeds, the worker returns only the exact declared output paths. Telchar checks metadata, references, NAR identity and structure, expected path set, and gateway-store registration before acknowledging each output.

Missing, extra, corrupt, duplicate, oversized, out-of-order, or rejected output data fails the build.

## Recovery and failure

Telchar persists the exact backend name, cluster endpoint, namespace, deterministic job ID, and admitted build specification. After restart it checks gateway outputs first; otherwise it adopts only that exact job on that exact backend.

Timeout and cancellation purge only the persisted deterministic job. Missing jobs, foreign identities, failed allocations, callback authentication errors, transfer failures, and unverifiable outputs become one terminal failure. Telchar does not submit a replacement job or move the build to another compatible backend.

## Cache publication

Telchar does not publish to a binary cache. Operators may use Attic, `nix copy`, or ordinary post-build tooling after gateway success. Publication failure must not change a build that Telchar has already validated and completed.

Cache credentials and trust policy stay outside client requests and outside generated Nix configuration.

## Configuration shape

A Nomad target controls its own endpoint, namespace, credentials, capacity, resources, driver, `driver_config`, store, transfer authentication, transfer limits, and optional prestart task. These settings are strict: unknown fields or unsafe credential files fail startup.

Consult `crates/telchar/tests/service_config.rs` for complete exercised TOML examples until a generated configuration reference exists.
