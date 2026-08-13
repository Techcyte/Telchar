# Nomad cache and log integration

Telchar does not implement a binary cache or historical log archive. Operators compose those services with the allocation-side Nix store and ordinary Nix tooling.

## Allocation store resolution

The Nomad worker receives the complete bounded admitted closure manifest. For every admitted path it:

1. checks the configured allocation-side Nix store;
2. allows that store's operator-configured substituters to resolve the path;
3. checks validity again;
4. requests the NAR from Telchar only when the admitted path remains unresolved.

The complete manifest is authorization authority. Cache availability changes which NAR bodies cross the callback connection, not which paths the worker may request.

### Host Nix daemon reuse

Mounting a host daemon socket is privileged Nomad operator policy. Telchar never injects that mount. Configure the backend store endpoint after arranging the matching task-driver mount:

```toml
[[backends.nomad]]
name = "nomad-primary"
system = "x86_64-linux"
maximum_concurrent_builds = 4
endpoint = "https://nomad.example:4646"
namespace = "telchar"
driver = "docker"
transfer_endpoint = "wss://build-transfer.example/callback"

[backends.nomad.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"
```

The operator remains responsible for daemon trust, socket permissions, mount isolation, and the host store's substituter policy.

### Local allocation store with substituters

A worker can instead use an allocation-local daemon or store. Configure substituters and trusted public keys in that Nix installation through ordinary `nix.conf` policy:

```text
substituters = https://cache.nixos.org https://cache.example
trusted-public-keys = cache.nixos.org-1:... cache.example-1:...
```

Cache URLs, credentials, keys, and trust policy never come from stock Nix client bytes. Private dependencies that remain unresolved fall back to selective Telchar transfer.

## Optional prestart configuration

Use the same-group Nomad lifecycle prestart task for bounded operator setup, including materializing an allocation-local `nix.conf` or authenticating an operator-selected cache client:

```toml
[backends.nomad.prestart]
driver = "raw_exec"
timeout_seconds = 120

[backends.nomad.prestart.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 256

[backends.nomad.prestart.driver_config]
command = "/opt/operator/bin/configure-nix"
args = ["/alloc/data/nix"]
```

No client interpolation is performed. Prestart failure prevents the build task from starting and terminates the attempt without automatic retry.

## Publishing outputs to an external cache

Gateway output import is the Telchar success boundary. Cache publication is separate and may occur afterward through existing Nix mechanisms:

- an operator-managed `post-build-hook`;
- `nix copy --to https://cache.example`;
- Attic's ordinary watcher or upload tooling.

Publication failure must not retroactively change a validated Telchar build result unless the operator deliberately places publication inside a backend build contract. Telchar stores no cache credentials and exposes no cache service.

## Live logs and archival

Build logs are bounded, connection-scoped delivery. PostgreSQL stores no log bytes. Consequences:

- a late follower receives only logs emitted after attachment;
- requester disconnect does not cancel detached execution;
- reconnect does not replay earlier logs;
- daemon restart does not recover historical logs;
- slow clients are bounded by configured live-log chunk and queue limits.

Operators needing archival should capture logs outside Telchar. The post-MVP extension seam is a bounded local zstd spool on durable storage, optionally uploaded by external tooling. Any such implementation must keep explicit byte limits, retention, cleanup, and credential ownership. Redis log storage and built-in object-storage clients remain out of scope.

## NixOS module composition

The public module accepts operator-selected backend tools without deciding cache policy:

```nix
{
  imports = [ inputs.telchar.nixosModules.default ];

  services.telchar = {
    enable = true;
    package = inputs.telchar.packages.${pkgs.system}.telchar;
    backendPackages = [ pkgs.nix pkgs.attic-client ];
    settings = {
      # Strict Telchar configuration, including backends.nomad.
    };
  };
}
```

Protected cache credentials should use systemd credentials or another operator-managed secret mechanism. Do not place secrets in `services.telchar.settings`, because generated Nix configuration is stored in the world-readable Nix store.
