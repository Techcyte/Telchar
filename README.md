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

The MVP supports classic input-addressed derivations in normal build mode. It includes durable PostgreSQL coordination, duplicate suppression, per-subject queue limits, exact-target restart recovery, bounded transfers, and client-independent execution.

Current limits:

- fixed-output and content-addressed derivations are not supported;
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

Run the complete release suite on a Nix-enabled Linux host:

```bash
./scripts/check-release.sh
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
- [Operator guide](docs/operations.md)
- [Nomad backend](docs/nomad.md)
- [Nix compatibility](docs/compatibility.md)
- [Roadmap](docs/roadmap.md)

## AI usage

Telchar's initial design was planned by a human with AI assistance, and most of the first implementation was written through an AI coding agent. The project is an experiment in whether careful up-front constraints and executable verification can make that workflow produce useful software.

## License

Telchar is licensed under the [MIT License](LICENSE).
