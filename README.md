# Telchar

Telchar dispatches Nix builds across a changing pool of local, SSH, and Nomad workers. It matches each build to compatible capacity, shares duplicate work, and keeps requests queued when workers are busy or unavailable.

Stock Nix clients connect over `ssh-ng` without plugins or patches.

```text
stock Nix client
  → OpenSSH forced command
  → Telchar daemon
  → local Nix, static SSH, or Nomad
  → validated outputs in the gateway store
  → normal Nix BuildResult
```

## Status

Telchar releases use `YYYY.M.PATCH` versions.

Telchar supports classic input-addressed and fixed-output derivations in normal build mode. It includes durable PostgreSQL coordination, duplicate suppression, gateway cache substitution, per-subject queue limits, exact-target restart recovery, bounded transfers, and client-independent execution.

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
    database.urlFile = "/run/secrets/telchar-database-url";
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

The module manages the Telchar daemon, account, runtime/state directories, application configuration, and credential inputs. PostgreSQL provisioning, host Nix permissions, retained GC-root directories, SSH ingress, and credential acquisition belong to the deployment. The configuration above assumes those dependencies already exist.

The flake exports `nixosModules.default` and `lib.sshForcedCommand`. [`examples/nixos`](examples/nixos/README.md) contains deployment compositions for local PostgreSQL, OpenSSH ingress, and AWS/Vault/Nomad.

Before production deployment, read the [operator guide](docs/operations.md). Nomad deployments also need the [Nomad guide](docs/nomad.md) and may use the jobspec in [`examples/nomad`](examples/nomad/README.md).

## Using a running gateway

### Configure the requesting client's SSH access

Obtain the gateway's SSH hostname, port, login user, client credentials, and trusted host key or host CA from its operator. The examples below use `build.example.com:2222` and login user `telchar`; replace them with your deployment's values. Telchar ingress is a forced-command Nix endpoint, not an interactive shell.

For multi-user Nix, remote-builder SSH connections normally originate from the **requesting machine's root-owned Nix daemon**, not your interactive user. Install credentials and SSH configuration for that account. A successful connection as your user does not prove the daemon can connect. Single-user installations use the account running Nix instead.

For a root-owned daemon, create a private directory for SSH control sockets:

```bash
sudo install -d -m 0700 /root/.ssh/control
```

Add a host-specific entry to `/root/.ssh/config` (preserving other host entries):

```sshconfig
Host build.example.com
    User telchar
    Port 2222
    IdentityFile /root/.ssh/telchar-client
    CertificateFile /root/.ssh/telchar-client-cert.pub
    IdentitiesOnly yes
    BatchMode yes
    StrictHostKeyChecking yes
    UserKnownHostsFile /root/.ssh/telchar-known-hosts
    ControlMaster auto
    ControlPath /root/.ssh/control/%C
    ControlPersist 60
```

Use the credential paths supplied by your operator. Omit `CertificateFile` only when the deployment accepts ordinary authorized keys rather than client certificates. Protect the private key and SSH configuration from other users; populate the known-hosts file with the operator-verified host key or host CA. Do not disable host verification to make a connection succeed.

Connection sharing is recommended for workloads containing many small derivations. See the [OpenSSH connection-sharing documentation](https://man.openbsd.org/ssh_config#ControlMaster).

Check store access using the same account and configuration as the daemon:

```bash
sudo nix --extra-experimental-features nix-command store info \
  --store ssh-ng://telchar@build.example.com:2222
```

### Submit a build

From a flake project with a default package:

```bash
nix build .#default \
  --max-jobs 0 \
  --builders 'ssh-ng://telchar@build.example.com:2222 x86_64-linux /root/.ssh/telchar-client 5 1' \
  --option builders-use-substitutes true
```

Replace `.#default` with your package attribute and `x86_64-linux` with a system supported by the gateway. Choose a remote slot count appropriate for deployment capacity. `--max-jobs 0` prevents local builds but still allows the client to fetch cached outputs. For persistent configuration and builder-field details, see the [Nix remote-build guide](https://nix.dev/manual/nix/stable/advanced-topics/distributed-builds).

Keep the gateway store separate from the requesting client's store. **Execution workers must not delegate their Nix builds back to the same Telchar gateway**: a worker can otherwise join its own in-progress build and wait on itself. In particular, inspect the host daemon's remote-builder configuration when a Nomad worker mounts its Nix daemon socket.

## Development

Run the sandbox-compatible flake evaluation and full integration suite:

```bash
NIXPKGS_ALLOW_UNFREE=1 nix flake check --impure --no-build
nix develop -c cargo test --locked --workspace -- --test-threads=1
```

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

OCI images are published as `ghcr.io/techcyte/telchar`, `telchar-nomad-worker`, `telchar-nix-daemon`, and `telchar-ssh-ingress`. Use an exact `YYYY.M.PATCH` tag for a release or `main` for the latest unreleased main-branch build.

The gateway and worker images are application runtimes. The Nix-daemon image provides an isolated gateway-store sidecar, and the optional SSH-ingress image provides restricted stock-Nix ingress. See the [operator guide](docs/operations.md) for deployment requirements and the [Nomad guide](docs/nomad.md) for worker configuration.

## Documentation

- [Architecture](docs/design.md)
- [Code tour](docs/code-tour.md)
- [Operator guide](docs/operations.md)
- [Nomad backend](docs/nomad.md)
- [Nix compatibility](docs/compatibility.md)
- [OTLP metrics](docs/metrics.md)
- [IPC and Nomad transfer messages](docs/ipc-and-transfer-messages.md)
- [Roadmap](docs/roadmap.md)

## License

Telchar is licensed under the [MIT License](LICENSE).
