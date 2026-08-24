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
- transfer endpoint: `ws://` directly, `ws://` through a Connect loopback upstream, or externally terminated `wss://`;
- required WebSocket subprotocol: `telchar-nomad-transfer-v1`.

Telchar's callback listener is plaintext WebSocket. Public `wss://` requires an operator-managed reverse proxy or load balancer. The proxy must preserve upgrades and the subprotocol, disable retries, and keep its idle timeout above Telchar's transfer idle timeout.

A backend may instead configure `callback_connect`. Telchar then renders bridge networking and a Consul Connect sidecar upstream into every generated execution group. The worker connects to the configured loopback `transfer_endpoint`; Envoy carries that connection over Connect mTLS to the gateway callback service. `source_service`, `destination_service`, and `local_bind_port` are operator authority. Consul intentions should allow only the generated source service to reach the gateway destination service. Connect protects and authorizes transport; workload identity or HMAC callback authentication remains mandatory and still binds the exact backend, job, allocation, and task.

The allocation opens the connection; Telchar never discovers Nomad client addresses or dials arbitrary allocations. The first binary TLNW message is `Authenticate`. Text messages, wrong subprotocols, invalid phases, and oversized messages fail closed. WebSocket is only transport; TLNW defines the authenticated session and transfer state.

## Authentication

Every transfer is authenticated, including on trusted plaintext networks.

### Workload identity

Configure a JWKS URL, audience, and optional CA certificate. Telchar verifies the signature and exact audience, namespace, job, allocation, and task claims. Issuer verification is disabled by default because Nomad omits the `iss` claim unless its servers configure `oidc_issuer`. To require it, configure both `issuer` and `verify_issuer = true`. Telchar does not infer identity trust from the Nomad API endpoint.

### HMAC capability

Telchar can sign a short-lived capability with a protected backend key. The allocation receives the scoped capability and an ephemeral request-signing key, not the backend signing key. The capability binds protocol version, backend, namespace, deterministic job, shared-build digest, expiry, and nonce. Callback authentication also verifies the worker-reported allocation against the exact configured Nomad job and task. Replay, expiry, scope, and request binding are checked before transfer.

HMAC provides authentication and integrity. On `ws://`, it does not hide tokens, paths, derivation metadata, logs, or NAR data from the network.

## Allocation-side Nix store

The worker uses the configured Nix store or daemon. Common choices are:

- a mounted host Nix daemon with a persistent warm store;
- an allocation-local daemon or store with ordinary substituters.

The gateway independently supports either a host daemon socket or a sibling Nomad task running a dedicated Nix daemon. For the sibling topology, bind persistent operator storage to `/nix/store` and `/nix/var/nix`, and bind the daemon-socket directory into Telchar. Keep the Nix database and store together across rescheduling. Telchar needs only the socket; it does not mount or mutate the sidecar store directly.

Mounting daemon sockets and persistent directories is privileged operator policy. Telchar does not create arbitrary mounts or allow clients to select the store.

For each path in the complete admitted closure manifest, the worker checks the allocation store and requests only paths that are still invalid. Ordinary allocation-side Nix configuration may provide warm or pre-populated paths before callback startup, but the current callback flow does not invoke substitution or local realization between its validity check and input request.

The manifest is transfer authority. Store availability changes traffic volume, not which paths may be requested. NARs are streamed in ordered, non-interleaved chunks with exact paths, offsets, sizes, and final markers. Empty chunks, gaps, overlap, interleaving, duplicate paths, early completion, late completion, and aggregate-limit violations fail closed. Transfer memory is bounded; output imports use bounded spooling and may retain a small complete NAR in memory before spilling to disk.

## Optional prestart task

A backend may add one Nomad lifecycle `prestart` task in the same job and task group. It has operator-controlled driver configuration, bounded resources, and a configured Nomad kill timeout. That value controls termination grace; it is not a standalone runtime deadline for the prestart command. Typical uses include preparing `nix.conf`, cache credentials, proxies, mounts, or allocation directories.

Client data is never interpolated into the prestart command or driver configuration. Failure prevents the build task from starting and terminates the attempt without retry.

## Logs and outputs

After the complete input closure is valid, the worker runs normal-mode `BuildDerivation` through its configured Nix daemon.

The worker emits bounded `LogChunk` frames during `BuildDerivation`. After protocol validation, the callback publishes each chunk to every process-local Nix client currently attached to the exact shared build. Leader and follower sessions receive chunks in callback order before the terminal result.

Each attached session has an independent byte-bounded queue controlled by `live_log_queue_bytes`. Callback and build execution threads never wait for client writes. When a slow session exceeds its queue, Telchar drops its oldest queued chunks and sends `\n[telchar: earlier build logs truncated]\n` before the retained chunks. A slow or disconnected client never fails the remote build. Log bytes remain live-only: Telchar does not store them in PostgreSQL or replay them after process restart or callback reconnect.

After `BuildDerivation` succeeds, the worker returns only the exact declared output paths. Fixed-output method, algorithm, digest, and Nix content-address metadata remain bound to the admitted build specification through the job and callback protocol. Telchar checks metadata, references, NAR identity and structure, expected path set, admitted content authority, and gateway-store registration before acknowledging each output.

Missing, extra, corrupt, duplicate, oversized, out-of-order, or rejected output data fails the build.

## Recovery and failure

Telchar persists the backend name, deterministic job ID, expected outputs, and admitted build specification. Callback replay records separately retain bounded allocation and nonce identity. It never stores secret credentials, capabilities, NAR bodies, or logs in PostgreSQL.

After restart it checks gateway outputs first; otherwise it resolves the persisted backend name against current operator configuration and adopts only the active attempt's deterministic job ID through that backend. Do not change a Nomad backend's endpoint or namespace under the same name while it owns in-flight work. The current worker does not reconnect after callback failure.

`max_retries` controls infrastructure retries for each Nomad backend. It defaults to `0` and counts retries after the initial attempt, so `max_retries = 2` permits at most three attempts. Each attempt has a distinct persisted ordinal and deterministic Nomad job ID. Missing jobs, failed allocations, and Nomad API transport failures may rotate to the next identity after bounded exponential backoff with jitter. The original BuildDerivation timeout covers submission, monitoring, backoff, and every retry; it never resets.

Timeout and cancellation purge only the active deterministic job and never retry. Callback-recorded build failures, transfer failures, invalid configuration, persistence failures, and client log delivery failures are terminal. A retry remains on the configured backend; Telchar does not move the build to another compatible backend. Stale callbacks from a replaced attempt fail identity resolution after the active job ID rotates.

## Cache publication

An optional bounded post-success executable may invoke operator tooling such as Attic or `nix copy`. Publication failure does not change a build Telchar has already validated and completed.

Cache credentials and trust policy stay outside client requests and outside generated Nix configuration. Telchar still does not implement a binary-cache protocol.

## Configuration shape

The callback listener is configured under `[backends.nomad_callback]`, not a top-level `[nomad_callback]` table. It is shared by all configured Nomad backends and is required whenever at least one `[[backends.nomad]]` target exists:

```toml
[backends.nomad_callback]
bind = "0.0.0.0:7443"
public_url = "wss://telchar.example.invalid/callback"
maximum_connections = 64
maximum_header_bytes = 16384
maximum_body_bytes = 65536
authentication_request_timeout_seconds = 10
shutdown_drain_timeout_seconds = 30
maximum_jwks_bytes = 1048576
```

A Nomad target controls its own endpoint, namespace, node pool, credentials, capacity, retry count, placement constraints, resources, driver, `driver_config`, store, transfer authentication, transfer limits, and optional prestart task. Set `node_pool` on `[[backends.nomad]]` to submit jobs to a specific Nomad node pool; when omitted, it defaults to Nomad's `default` pool.

```toml
[[backends.nomad]]
namespace = "telchar"
node_pool = "builders"
maximum_concurrent_builds = 4
max_retries = 2
```

`maximum_concurrent_builds` limits logical builds using the backend; a retry keeps that logical build's permit while replacing its Nomad execution attempt. `max_retries` is bounded to `100` and should normally remain small.

Placement constraints are operator-supplied Nomad left target, operand, and right target values rendered directly into each generated job:

```toml
[[backends.nomad.constraints]]
attribute = "${attr.cpu.arch}"
operator = "="
value = "amd64"

[[backends.nomad.constraints]]
attribute = "${node.class}"
operator = "="
value = "general"
```

The required `[backends.nomad.resources]` table remains the backward-compatible default resource profile. Its priority defaults to `50`; an operator may bound it explicitly:

```toml
[backends.nomad.resources]
cpu_mhz = 2000
memory_mb = 4096
disk_mb = 16384

[backends.nomad.priority]
minimum = 40
default = 50
maximum = 60
```

Additional profiles map one operator-advertised Nix system feature to resources, bounded priority policy, and optional additive placement constraints:

```toml
# The selector must also be advertised by supported_features.
supported_features = ["big-parallel", "overflow-aws"]

[[backends.nomad.resource_profiles]]
name = "overflow"
required_feature = "overflow-aws"
cpu_mhz = 4000
memory_mb = 8192
disk_mb = 32768
priority_minimum = 50
priority_default = 60
priority_maximum = 70

[[backends.nomad.resource_profiles.constraints]]
attribute = "${node.class}"
operator = "="
value = "aws-overflow"
```

Selection is deterministic: no mapped feature selects `default`, exactly one mapped feature selects that profile, and multiple mapped profile features fail as ambiguous. Unrecognized required features remain ordinary backend incompatibility. Base backend constraints always remain in force; selected profile constraints are appended. A requested feature is workload placement input, not authenticated authorization. Operators retain authority through explicit feature advertisement, mappings, hard resource and priority bounds, placement constraints, and queue/concurrency limits.

The selected profile's `priority_default` is rendered into the Nomad job. The configured minimum and maximum reserve bounded policy for later operator-controlled adjustments; derivations cannot supply a numeric priority. Nomad priority affects preemption when enabled and does not order pending jobs. Telchar's dependency readiness and durable admission remain separate gates.

Profiles never infer CPU, memory, priority, or placement from derivation size, closure size, input count, or presumed workload cost. Placement constraints can select GPU-capable nodes but do not reserve a GPU. Bounded Nomad device reservations are deferred on the roadmap.

Limits bound profile and constraint counts and field sizes, manifest count and bytes, individual and aggregate NAR sizes, metadata, buffers, per-frame log bytes, per-attached-session live-log queues, idle time, total runtime, connection lifetime, authentication, replay retention, and diagnostics. The callback enforces setup and output-collection phase deadlines in addition to the total connection lifetime. Reconnect limits remain strict configuration input but are not yet enforced because the current worker does not reconnect. Unknown fields or unsafe credential files fail startup.

Consult `crates/telchar/tests/service_config.rs` for complete exercised TOML examples until a generated configuration reference exists.
