# Operator guide

Telchar is a trusted Nix gateway, not a tenant boundary. Run it on a dedicated Linux host or VM with a dedicated gateway store and one PostgreSQL database.

The public NixOS module is `nixosModules.default` (also exported as `nixosModules.telchar`). It can manage the daemon, local PostgreSQL, trusted gateway-store access, and restricted OpenSSH ingress.

## Before deployment

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

Minimal local-backend configuration:

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

The module enables local PostgreSQL, gateway Nix-daemon access, and OpenSSH ingress unless their `enable` options are disabled. `services.telchar.settings` is rendered as strict TOML. Backend helper programs can be added with `services.telchar.backendPackages`.

The module's forced command derives the accepted key fingerprint from the configured `authorized_keys` file. Keep that file operator-owned and restricted to the Telchar account.

## PostgreSQL and recovery

Only one daemon may own a deployment database. A second daemon refuses startup. Loss of the ownership connection fences the running daemon, removes its IPC socket, and exits unsuccessfully. Restart only after PostgreSQL is authoritative again; the replacement acquires a fresh lifetime lock.

Back up PostgreSQL with a PostgreSQL-aware tool such as `pg_dump -Fc`. The backup must preserve:

- the complete migration ledger;
- shared-build rows and admitted build specifications;
- attempt and backend execution identity;
- transfer, retention, attachment, and terminal metadata.

PostgreSQL must not contain NAR bodies, credentials, capabilities, signatures, or build logs. Back up the gateway Nix store and GC-root directory separately. A database-only restore does not restore missing store objects.

Recovery checks exact gateway-store outputs first. Static SSH recovery remains bound to the original target. Nomad recovery remains bound to the original backend, namespace, and job identity. Missing or unverifiable state fails closed; Telchar does not resubmit automatically.

Failure procedure:

1. stop client ingress or let requests fail closed;
2. preserve PostgreSQL, the gateway store, GC roots, and import spool before changing state;
3. restore PostgreSQL and store state from the same recovery point;
4. verify the gateway Nix daemon socket and backend credentials;
5. start one Telchar daemon and confirm ownership acquisition, migration completion, and recovery telemetry;
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

## Logs and telemetry

Build logs are bounded and live-only. Late followers, reconnecting clients, and restarted daemons do not receive earlier log output. PostgreSQL stores no log bytes.

Send systemd journals and OTLP signals to operator-owned systems. Telchar supports OTLP/gRPC and OTLP/HTTP with protobuf encoding:

```bash
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4317
```

Use `http/protobuf` and port `4318` for OTLP/HTTP. Unsupported protocols fail startup. Telchar exposes no Prometheus endpoint. Telemetry is bounded and omits protocol bodies, NAR contents, secrets, raw authentication material, request identities, derivation paths, and execution identities from metric attributes.

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

Verification commands:

```bash
nix develop -c cargo fmt --all -- --check
nix develop -c cargo check --locked --workspace
nix develop -c cargo clippy --locked --workspace --all-targets -- -D warnings
nix develop -c cargo test --locked --workspace -- --test-threads=1
NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build
```

Build release artifacts and selected VM checks directly from their flake attributes. The suite covers packages, the public NixOS module, stock-Nix local and fixed-output builds, static SSH, Nomad, duplicate coalescing, requester disconnect, output reuse, and restart recovery.

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
