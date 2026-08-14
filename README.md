# Telchar

Telchar is a self-hosted Nix build gateway. Stock Nix clients connect over `ssh-ng`; Telchar validates each build, coalesces duplicate requests, queues work fairly, and runs it on a compatible local, SSH, or Nomad backend.

Clients need no plugin or patched Nix installation.

```text
stock Nix client
  → OpenSSH forced command
  → Telchar daemon
  → local Nix, static SSH, or Nomad
  → validated outputs in the gateway store
  → normal Nix BuildResult
```

## Status

The MVP supports classic input-addressed and fixed-output derivations in normal build mode. It includes durable PostgreSQL coordination, duplicate suppression, gateway cache substitution, per-subject queue limits, exact-target restart recovery, bounded transfers, and client-independent execution.

Current limits:

- floating content-addressed derivations are not supported;
- authenticated clients share one trusted store domain;
- builds are not retried automatically;
- logs are live and bounded, with no replay;
- deployments are single-active;
- Telchar does not terminate TLS or provide a binary cache.

See [compatibility](docs/compatibility.md) and the [roadmap](docs/roadmap.md) for details.

## Quick start

Telchar is packaged through the flake and includes a NixOS module:

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
        maximum_concurrent_builds = 4;
      };
    };
  };
}
```

The module enables a local PostgreSQL database, trusted gateway-store access, and restricted OpenSSH ingress by default. Add client keys to `/var/lib/telchar/.ssh/authorized_keys`, or set `services.telchar.openssh.authorizedKeysFile` to another operator-managed file.

A stock Nix client can then use the gateway as a remote builder:

```bash
nix build \
  --max-jobs 0 \
  --builders 'ssh-ng://telchar@build-host x86_64-linux'
```

The gateway must have its own Nix store. Do not point a local client and Telchar at the same host store; recursive store locking can deadlock the build.

Before production deployment, read the [operator guide](docs/operations.md). Nomad deployments also need the [Nomad guide](docs/nomad.md).

## Development

Run the sandbox-compatible flake checks and the full integration suite:

```bash
nix flake check
nix develop -c cargo test --locked --workspace
```

Useful packages:

```bash
nix build .#telchar
nix build .#telchar-nomad-worker
```

Reproducible OCI image archives are also flake packages:

```bash
nix build .#telchar-oci
nix build .#telchar-nomad-worker-oci
podman load < result
```

The gateway image starts `telchar daemon`; the worker image starts `telchar-nomad-worker`. Runtime configuration, credentials, PostgreSQL, gateway-store access, and callback networking remain operator responsibilities.

## Documentation

- [Architecture](docs/design.md)
- [Code tour](docs/code-tour.md)
- [Operator guide](docs/operations.md)
- [Nomad backend](docs/nomad.md)
- [Nix compatibility](docs/compatibility.md)
- [OTLP metrics](docs/metrics.md)
- [Roadmap](docs/roadmap.md)

## AI Usage Disclosure

The original implementation for Telchar was planned out by a human with AI assistance for all the features. Actual coding took place almost entirely with an AI agent for the initial implementation. I was curious if I could actually make something useful by planning something out before-hand and then giving that to an LLM to completely implement (with guidance when ambiguity came up), time will tell if that was a horrible idea or not.

## License

Telchar is licensed under the [MIT License](LICENSE).
