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

Telchar uses SemVer-compatible calendar versions in `YYYY.M.PATCH` form. OCI archives always carry the package version; only manually approved releases are published.

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

Before production deployment, read the [operator guide](docs/operations.md). Nomad deployments also need the [Nomad guide](docs/nomad.md). A heavily commented, cluster-independent jobspec is available in [`examples/nomad`](examples/nomad/README.md).

## Development

Run the sandbox-compatible flake evaluation and full integration suite:

```bash
NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build
nix develop -c cargo test --locked --workspace -- --test-threads=1
```

For curated release verification, including package builds and selected VM contracts, run `./scripts/check-release.sh`.

The flake currently publishes and tests `x86_64-linux` outputs only.

Useful packages:

```bash
nix build .#telchar
nix build .#telchar-nomad-worker
```

Reproducible OCI image archives are also flake packages:

```bash
nix build .#telchar-oci
nix build .#telchar-nix-daemon-oci
nix build .#telchar-nomad-worker-oci
nix build .#telchar-ssh-ingress-oci
```

Releases use two manual GitHub Actions workflows. **Prepare release** increments the patch when the package is already versioned for the current month, otherwise starts the month at `YYYY.M.0`; it updates all Cargo/Nix version authorities and opens a reviewable pull request. After that pull request is merged, **Publish release** verifies the prepared revision, publishes all four archives to `ghcr.io/techcyte`, and creates the matching GitHub Release and `vYYYY.M.PATCH` tag. Images receive only the exact version tag; no moving `latest` tag is published.

The gateway and worker images are the application runtimes. The Nix-daemon image provides an isolated gateway-store sidecar, and the optional SSH-ingress image provides restricted stock-Nix ingress. Load or publish the exact archives with your container tooling.

The gateway image runs as `995:995` with `HOME=/var/lib/telchar` and starts `telchar daemon --socket /run/telchar/daemon.sock --frontend-uid 995`; override the user, home, and command together when using another UID. Mount `/etc/telchar`, `/run/telchar`, persistent import and GC-root state, and the gateway Nix daemon socket. Supply PostgreSQL, credentials, configuration, and OTLP settings through operator-owned files or environment variables. The same binary provides bounded JSON inspection through `telchar operator status`, `queue`, `build`, `backends`, `recovery`, and `config-check`; see the [operator guide](docs/operations.md#read-only-operator-cli). The worker image starts `telchar-nomad-worker` and expects the bounded Nomad allocation environment documented in [Nomad backend](docs/nomad.md).

Executable release coverage validates and loads both application archives into Docker. The OCI runtime check verifies image entrypoints and bounded startup behavior, while the gateway VM exercises the gateway image as a non-root process against PostgreSQL and a real Nix daemon. Together the selected checks prove stock-Nix classic and fixed-output builds, retained-result reuse, graceful and crash restart, gateway-store interruption, PostgreSQL ownership fencing, exact-archive redeployment, idempotent migration, future-schema rejection, and no blind resubmission.

## Documentation

- [Architecture](docs/design.md)
- [Code tour](docs/code-tour.md)
- [Operator guide](docs/operations.md)
- [Nomad backend](docs/nomad.md)
- [Nix compatibility](docs/compatibility.md)
- [OTLP metrics](docs/metrics.md)
- [IPC and Nomad transfer messages](docs/ipc-and-transfer-messages.md)
- [Roadmap](docs/roadmap.md)

## AI Usage Disclosure

The original implementation for Telchar was planned out by a human with AI assistance for all the features. Actual coding took place almost entirely with an AI agent for the initial implementation. I was curious if I could actually make something useful by planning something out before-hand and then giving that to an LLM to completely implement (with guidance when ambiguity came up), time will tell if that was a horrible idea or not.

## License

Telchar is licensed under the [MIT License](LICENSE).
