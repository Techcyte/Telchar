# Operator guide

Telchar is a trusted Nix gateway, not a tenant boundary. Run it on a dedicated Linux host or VM with a dedicated gateway store and one PostgreSQL database.

The public NixOS module is `nixosModules.default` (also exported as `nixosModules.telchar`). It can manage the daemon, local PostgreSQL, trusted gateway-store access, and restricted OpenSSH ingress.

## Before deployment

Telchar supports two gateway Nix-daemon topologies:

- **Host passthrough** connects to the host daemon through its Unix socket. A NixOS service can use the normal host socket directly; a container can bind-mount that socket. The host daemon must authorize the numeric runtime UID as a trusted user.
- **Sidecar passthrough** connects to a sibling Nix daemon through a shared Unix socket. Persist `/nix/store` and `/nix/var/nix` outside the allocation, keep the daemon and Telchar runtime identities coherent, and do not back up the Nix store with file-oriented backup jobs. Preserve store state and its database as one unit when recovery requires a backup.

`TELCHAR_GATEWAY_STORE_URI` selects the operator-owned daemon socket. Clients cannot select or alter this topology.

- Use a gateway store that is not shared with a local client workload.
- Give the daemon access only to its PostgreSQL database, gateway Nix daemon, state directories, and configured backend credentials.
- Keep the daemon Unix socket private to the configured frontend UID.
- Pin static SSH host keys.
- Put secrets in `services.telchar.credentials` or another protected file mechanism. Nix-generated configuration is world-readable.
- Treat all authenticated clients as members of one shared store domain.
- Configure each backend's name, systems, features, capacity, credentials, and timeouts explicitly.

The default module paths are:

```text
/run/telchar/daemon.sock
/var/lib/telchar/.ssh/authorized_keys
/var/lib/telchar/gc-roots
```

## NixOS module

Minimal local-backend service configuration, assuming PostgreSQL, gateway-store permissions, and the GC-root directory are provisioned separately:

```nix
{
  imports = [ inputs.telchar.nixosModules.default ];

  services.telchar = {
    enable = true;
    package = inputs.telchar.packages.${pkgs.system}.telchar;
    settings = {
      backends.local = {
        name = "local";
        system = pkgs.system;
        supported_features = [ ];
        maximum_concurrent_builds = 4;
      };
    };
  };
}
```

The low-level module manages only the Telchar daemon by default. Infrastructure management is explicitly opt-in:

- `services.telchar.database.manage = true` provisions local PostgreSQL coordination.
- `services.telchar.gatewayStore.manageTrustedUser = true` adds the service user to the host Nix trusted-users list.
- `services.telchar.gatewayStore.manageGcRootDirectory = true` creates the configured GC-root directory.
- `services.telchar.ingress.openssh.enable = true` enables dedicated restricted SSH ingress without altering the host's regular OpenSSH service.

The `nixosModules.standalone` profile enables Telchar, trusted-user and GC-root directory management, and SSH ingress. It does not enable local PostgreSQL management; configure an external database or opt into `database.manage` separately.

`services.telchar.settings` is rendered as strict TOML. Backend helper programs can be added with `services.telchar.backendPackages`.

When SSH ingress is enabled, the module enables OpenSSH authentication metadata. Public keys produce fingerprint-based credential IDs. Certificates produce `ssh-cert:<CA-byte-length>:<CA-fingerprint>:<key-ID-byte-length>:<key-ID>` credential IDs, with certificate principals retained as metadata. NixOS and OCI ingress use the same extraction and normalization. Explicit identity mappings override audit and quota subjects; otherwise the first certificate principal supplies the audit subject and the credential ID supplies the quota subject. Keep authentication files operator-owned and restricted to the Telchar account.

Server identity and client authentication are independent. `ingress.openssh.hostCertificateFile` adds a server certificate without requiring client certificates. `trustedUserCAKeysFile` enables client certificates with or without a server certificate; `authorizedPrincipalsFile` optionally restricts principals. `authorizedKeysFile` remains available alongside the client CA.

Credential paths can be symlinks to regular files owned by root or the service UID. Private targets must deny group/other access; public targets must deny group/other writes. Protect the containing directories and rotation mechanism too: external programs such as SSH reopen paths when invoked.

### Optional Vault AWS provisioning

Import `inputs.telchar.nixosModules.vaultAws` alongside the service or standalone module only when Vault should provision credentials. The base module has no Vault or AWS requirement. Configure `services.telchar.vaultAwsAuth.enable`, `address`, and `role`; EC2 instance-profile credentials authenticate through IMDSv2.

- `sshSignPath` enables host certificate issuance and requires `sshHostCertificateRenewal`.
- `nomadSecretPath` enables Nomad token issuance and requires `nomad.renewal`.
- Either path can be omitted. Other credentials can come from independent provisioning mechanisms.
- Optional `hostCAPath` and `clientCAPath` deliver CA public keys to the configured consumer paths. Omit them to keep operator-provisioned trust anchors.
- `authMount`, `metadataEndpoint`, `stsEndpoint`, and `region` select provider endpoints and signing configuration.
- `renewal.enable` schedules fetching and installation for configured capabilities.

Without this module, candidate files and renewal units accept credentials delivered by operator-controlled tooling.

## PostgreSQL and recovery

`services.telchar.database.urlFile` selects a protected connection-string source, not a transport policy. It supports Unix-socket connections as well as remote endpoints. Setting `database.rootCertificateFile` additionally enables the startup validator requiring verified PostgreSQL TLS with that CA; it requires `urlFile`. Without that option, the connection string controls transport security.

Only one daemon may own a deployment database. Ownership is a PostgreSQL lease renewed every five seconds by default and valid for twenty seconds by default. Configure `database.ownership_renewal_seconds` and `database.ownership_lease_seconds`; the lease duration must be at least three renewal intervals. A second daemon refuses startup while the lease is current. After expiration, a replacement acquires a higher fencing generation. PostgreSQL rejects durable mutations from expired generations, and a fenced daemon removes its IPC socket and exits unsuccessfully on its next renewal. This remains safe across process, node, proxy, and network loss without manual PostgreSQL session termination.

Back up PostgreSQL with a PostgreSQL-aware tool such as `pg_dump -Fc`. The backup must preserve:

- the complete migration ledger;
- shared-build rows and admitted build specifications;
- attempt and backend execution identity;
- transfer, retention, attachment, and terminal metadata.

PostgreSQL must not contain NAR bodies, secret credential material, signatures, or build-log bytes. It does contain bounded credential identifiers and authentication authority, backend capability metadata, admitted build specifications, and execution identities required for scheduling and recovery. Back up the gateway Nix store and GC-root directory separately. A database-only restore does not restore missing store objects.

Recovery checks exact gateway-store outputs first. Static SSH recovery remains bound to the original target. Nomad recovery uses the persisted backend name and deterministic job identity, resolving the backend through current configuration; do not change endpoint or namespace under the same name while work is in flight. Missing or unverifiable state fails closed; Telchar does not resubmit automatically.

## Static SSH readiness

Telchar immediately checks every configured static SSH backend during startup by opening its pinned, noninteractive SSH connection and completing the Nix worker-protocol handshake with `nix-daemon --stdio`. An unavailable host does not prevent daemon startup. It remains excluded from new backend selection until a later check succeeds.

Ready hosts are checked every five minutes by default. Unavailable hosts are checked every minute so machines returning to the network become eligible quickly:

```toml
[[backends.ssh]]
ready_check_interval_seconds = 300
unavailable_check_interval_seconds = 60
check_timeout_seconds = 10
identity_file = "/run/secrets/telchar-builder-key"
known_hosts_file = "/etc/telchar/ssh-known-hosts"

[backends.ssh.fixed-builders]
source = "static"

[backends.ssh.fixed-builders.builder-1]
address = "builder-1.example"
```

All values must be positive and bounded. A failed check covers network, host-key, authentication, remote-command, and Nix protocol failure as one `unavailable` state. Telchar does not retry or migrate work after dispatch; a host can still disappear between its successful check and build execution. Exact-target recovery is unchanged.

Send `SIGHUP` to the daemon after atomically replacing its configuration file to add static SSH leaves or after replacing the contents of an unchanged Nomad `token_file`. Reload parses and validates the complete file, rereads Nomad credential files while assembling replacement clients, immediately probes the resulting static SSH inventory, and publishes one immutable backend generation for subsequently accepted sessions. Existing sessions and in-flight builds retain their previous generation.

Nomad token rotation changes only the protected file contents, not the configured path or backend definition. A Nomad template may render a short-lived Vault-issued token to that path with `change_mode = "signal"` and `change_signal = "SIGHUP"`. Invalid or unreadable replacement credentials reject the reload and leave the active generation serving work.

Reload treats the static SSH list as desired inventory. Added hosts are probed immediately. Omitted hosts are immediately excluded from every new selection, including requests on sessions accepted before the reload; work already assigned to an omitted host retains its exact immutable backend generation and may finish or fail normally. Backend permit acquisition uses that exact selected target rather than choosing another compatible host. Once those sessions finish, the omitted configuration disappears with the retired generation. Drain state is process-local and is not restored after daemon restart.

Changing an existing host under the same backend name, changing local or Nomad backends, or changing any non-backend setting remains unsupported. Such a reload is rejected while the active configuration continues serving work. Newly added but unavailable hosts are accepted in degraded state and remain excluded from scheduling until a readiness check succeeds.

NixOS automatically recovers SSH ingress while the daemon is active. Stopping only `telchar-sshd.service` does not keep ingress closed. For a persistent ingress shutdown, disable `services.telchar.ingress.openssh.enable` in the applied NixOS configuration.

Failure procedure:

1. stop client ingress or let requests fail closed;
2. preserve PostgreSQL, the gateway store, GC roots, and import spool before changing state;
3. restore PostgreSQL and store state from the same recovery point;
4. verify the gateway Nix daemon socket and backend credentials;
5. start one Telchar daemon and confirm ownership acquisition, fencing generation, migration completion, and recovery telemetry;
6. verify durable attempt counts before reopening ingress.

A gateway-store interruption rejects store-dependent operations. Restore the Nix daemon first, then replace the Telchar process so all long-lived store clients reconnect cleanly.

## Stores and retention

The gateway Nix daemon is trusted authority. The Telchar account needs the operations required for closure queries, NAR import/export, builds, substitution through `EnsurePath`, and GC-root retention. Substituters, cache credentials, trusted keys, signature policy, and store registration remain Nix-daemon configuration.

Keep the GC-root directory on persistent storage. Monitor it together with the gateway store, import spool, and disk reserve. Output retention defaults to a bounded period so a disconnected client can still retrieve a completed result.

Nomad allocation stores are separate operator policy. See [Nomad backend](nomad.md).

## Ingress and clients

Expose Nix ingress only through the restricted Telchar OpenSSH account. Disable passwords, PTYs, forwarding, user environment, and arbitrary commands.

Example client configuration:

```bash
nix build \
  --max-jobs 0 \
  --builders 'ssh-ng://telchar@build-host x86_64-linux'
```

The Nix builder entry describes the requested system and features. It cannot select a Telchar backend, cluster, store, credential, driver, quota, or cache policy.

Requester disconnect normally leaves admitted execution running. Followers share the same execution and do not consume another execution slot or backend permit.

## Cache publication

Optional post-success publication is configured in strict TOML:

```toml
[cache_publication]
executable = "/run/current-system/sw/bin/nix"
arguments = ["copy", "--to", "https://cache.example"]
timeout_seconds = 300
maximum_input_bytes = 65536
```

Telchar invokes the absolute executable directly without a shell and sends a JSON array of validated output paths on standard input. Arguments, input size, and runtime are bounded; subprocess output is suppressed. Publication is asynchronous and best effort: failure emits telemetry but cannot change a successful `BuildResult`. Credentials and cache trust policy belong to operator process configuration, never client bytes.

## Read-only operator CLI

The `operator` command reads strict configuration and durable PostgreSQL state. It does not acquire daemon ownership, run migrations, submit work, retry builds, cancel work, or contact backends. Output is one bounded JSON value for local automation:

```bash
telchar operator config-check
telchar operator status
telchar operator queue --limit 64
telchar operator build /nix/store/…-example.drv
telchar operator backends
telchar operator recovery --limit 64
```

`config-check` validates configuration without requiring PostgreSQL. Other commands require the configured database and an already migrated schema. `queue` reports durable queue order. `build` reports one derivation's durable state and current attempt. `backends` combines configured capacity with active durable builds and performs the same bounded SSH/Nix readiness probe for static SSH entries. `recovery` reports bounded active work and its persisted recovery mode. Limits default to 64 and may not exceed 256.

## Logs and telemetry

Build logs are bounded and live-only. Late followers, reconnecting clients, and restarted daemons do not receive earlier log output. PostgreSQL stores no log bytes.

Send systemd journals and OTLP signals to operator-owned systems. Telchar supports OTLP/gRPC and OTLP/HTTP with protobuf encoding:

```bash
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4317
```

Use `http/protobuf` and port `4318` for OTLP/HTTP. Unsupported protocols fail startup. Telchar exposes no Prometheus endpoint. Telemetry is bounded and omits protocol bodies, NAR contents, secrets, raw authentication material, request identities, derivation paths, and execution identities from metric attributes.

### Worker telemetry

The gateway and allocation worker link `telchar-telemetry`. Their resource service names
are `telchar` and `telchar-nomad-worker`; each reports its binary package version.
Both use the OTLP protocol and endpoint settings above, defaulting to gRPC on
`http://127.0.0.1:4317` (HTTP/protobuf defaults to port 4318). Local events go to
stderr, independently of the collector. Export queues and shutdown are bounded;
collector unavailability does not change a build result.

Worker INFO reports lifecycle phases, input/output summaries, counts and durations.
A phase still running after ten seconds emits periodic `worker.phase.running`
events. These indicate liveness, not a percentage of build work completed.
The reporter stops before its phase completion event. Phase duration metrics use
only phase and outcome dimensions, never paths or allocation IDs.

```bash
RUST_LOG=info,telchar_nomad_worker=debug
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318
```

Worker DEBUG includes full store paths, per-path transfer details and builder
output, without a separate opt-in. Builder output is arbitrary build-script text;
DEBUG copies it into local and exported diagnostics. Authentication credentials
and environment dumps are not added by worker instrumentation. Existing client
build-log delivery is independent of the tracing filter and remains live-only.
These variables must be set in the worker process environment, not only on the gateway.

### Server build lifecycle

Server INFO records shared-build leader/follower selection, durable result reuse,
queue entry and admission. `server.phase.started`, `server.phase.completed`, and
`server.phase.failed` describe `queue`, `substitute`, `execute`, `follow`,
`await-terminal`, and `validate-outputs` operations. Durations use monotonic time;
failures include error kind rather than arbitrary error text. Phase success means
the operation returned normally: substitution can still miss, and a follower wait
can return a failed shared result. Existing build completion/failure events report
the terminal outcome.

Long phases emit `server.phase.running` every ten seconds without catch-up bursts.
These indicate waiting or ongoing work, not measured build progress. Reporters stop
before the phase completion/failure event. For Nomad, `execute` includes placement,
worker execution and callback output collection; it does not assert the builder
is already running.

`RUST_LOG=info,telchar=debug` includes `server.build.paths` and
`server.build.output`. Nomad output is copied once at callback receipt rather than
once per attached client; local/static-SSH output is copied at the execution callback.
DEBUG text can contain arbitrary builder output, including secrets printed by builds.
Client log forwarding is independent of these diagnostic copies. No NAR payloads,
capability tokens, argv, or environment dumps are added by this instrumentation.

### Upload latency traces

Operation-level TRACE spans and events cover frontend connection/envelope/relay boundaries,
worker opcode reception, QueryValidPaths decoding, daemon batch queries,
daemon connection/handshake, and response write/flush. Enable selected targets in the
**server process** environment, not just the calling Nix client:

```bash
RUST_LOG=info,telchar::store=trace,telchar::service=trace,telchar::runtime=trace
journalctl -u telchar.service -o short-monotonic > query-trace.log
```

For NixOS, set `services.telchar.environment.RUST_LOG` to that filter. The SSH
frontend is a separate process: its server-owned forced-command environment or
sshd `SetEnv` must also set the filter. Frontend stderr travels over SSH to the
client; capture `nix copy ... 2>frontend-trace.log`. Never redirect frontend
stdout, which carries the Nix protocol, into diagnostic output.

Events carry monotonic `elapsed_us` durations. Opcode-read duration includes client
idle time; daemon query duration includes any requested substitution; response
flush duration is cumulative since response writing began. Console timestamps have
second resolution; use journal timestamps plus duration fields rather than
subtracting console timestamps. The frontend envelope event supplies `session_id`
to correlate with the daemon session. Frontend and daemon OTLP trace IDs are
separate: the IPC envelope does not propagate an OpenTelemetry parent context.

The same spans export through the configured OTLP endpoint. An operator-owned
OpenTelemetry Collector can route `traces` to its `debug` exporter for console
inspection or a trace backend for duration queries. Local journal capture requires
no collector; absent collectors may produce exporter diagnostics. TRACE adds work
when enabled, so compare timings with logging disabled before claiming throughput
improvements. Events omit payloads, paths, argv, environment values, and subprocess
stderr; counts and bounded status fields remain available. Relay events occur at
first bytes and completion, not per buffer.

`checks.x86_64-linux.nixos-query-trace` captures fresh one-path and 100-derivation
uploads plus independent missing-path CLI comparisons in an isolated NixOS VM.
Its diagnostic output includes intentionally captured and checked Nix DNS/cache
failure messages, and its output directory retains `query-syscalls`. Missing-path
`nix path-info` can probe configured substituters even though Telchar only needs
local validity; cache failures can therefore dominate the subprocess wait phase.

See [OTLP metrics](metrics.md) for instrument names, dimensions, autoscaling signals, and interpretation.

## TLS and callbacks

Telchar does not terminate TLS. For a public Nomad callback URL, place a reverse proxy or load balancer in front of the plaintext callback listener.

The proxy must:

- preserve WebSocket upgrades;
- preserve `telchar-nomad-transfer-v1`;
- disable automatic request retries;
- allow one connection for the configured maximum lifetime;
- keep the proxy-to-Telchar hop local or on a trusted network.

TLS keys belong to the proxy. Workload identity or HMAC authentication is still required. HMAC over plaintext authenticates messages but does not make their contents confidential.

## Upgrades and release checks

For the current alpha, qualify each exact OCI archive produced by the revision being deployed. Do not infer compatibility from an image tag.

Deployment procedure:

1. record the archive digest or loaded image ID;
2. create a PostgreSQL custom-format backup;
3. preserve the gateway store, GC roots, and import spool;
4. confirm backend credentials and exact Nomad namespaces remain available;
5. run the release suite for the candidate revision;
6. stop the active daemon cleanly;
7. load the exact archive and start one replacement daemon;
8. confirm migration completion, singleton ownership, schema version, and unchanged durable attempt counts;
9. remove a client-side result and verify the gateway reuses the retained result without another backend attempt.

Telchar rejects an unknown future schema version. Before a migration is applied, rollback means replacing the container with the previously retained artifact. After a migration is applied, changing the image alone is not rollback. Use proven schema compatibility or restore PostgreSQL and store state from the coordinated pre-deployment recovery point.

Run the curated release verification:

```bash
./scripts/check-release.sh
```

The script runs formatting, workspace checks, Clippy, serial integration tests, package builds, and selected NixOS workload and recovery contracts. `NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build` is useful as an evaluation-only check; it does not build or execute the release suite.

## Unsupported expectations

Do not rely on Telchar for:

- hostile tenant isolation;
- automatic build retries;
- log replay;
- Telchar-owned binary-cache protocols;
- floating content-addressed derivations;
- active/active scheduling;
- client-selected infrastructure;
- native TLS termination.
