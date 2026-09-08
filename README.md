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

Telchar uses SemVer-compatible calendar versions in `YYYY.M.PATCH` form. OCI archives always carry the package version. Every successful `main` build publishes the moving `main` image tag; manually approved releases publish exact version tags.

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

`nixosModules.telchar` and its `nixosModules.default` alias expose the same service module. `lib.sshForcedCommand` supplies Telchar's authenticated stdio adapter for operator-owned OpenSSH configuration. No standalone-host or Vault module is exported.

[`examples/nixos`](examples/nixos/README.md) contains explicit compositions for local PostgreSQL and regular OpenSSH, dedicated ingress, and an advanced AWS/Vault/Nomad deployment. Copy the pieces your deployment needs; examples are not automatically enabled service behavior.

Product checks under `checks.x86_64-linux` include `nixos-service-boundary`, `nixos-module`, `nixos-nomad-gateway`, `nixos-nomad-credential`, `nixos-nomad-credential-reload`, and `nixos-restart-reconciliation`. Vault and deployment credential-installation checks belong to telchar-gateway.

`nixos-module` covers protected PostgreSQL TLS configuration, service startup, SSH ingress, and delivery to an OTLP collector. Certificate rejection cases also run against real PostgreSQL processes in `crates/telchar/tests/persistence_migrations.rs`. `nixos-nomad-gateway` covers rejection of unauthenticated callbacks alongside authenticated builds.

Fixture-only diagnostics remain opt-in under `legacyPackages.x86_64-linux.fixtureChecks`: `nixos-artifacts`, `nixos-nomad-fixture`, and `nixos-static-ssh-fixture`. They validate test infrastructure, not product acceptance, and are excluded from `nix flake check`. For example:

```bash
nix build --no-link .#legacyPackages.x86_64-linux.fixtureChecks.nixos-artifacts
```

Before production deployment, read the [operator guide](docs/operations.md). Nomad deployments also need the [Nomad guide](docs/nomad.md). A heavily commented, cluster-independent jobspec is available in [`examples/nomad`](examples/nomad/README.md).

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

Connection sharing avoids repeating SSH connection establishment and authentication for every small derivation. `ControlMaster` shares an SSH transport, not a Nix build result or daemon session. `ControlPersist 60` keeps the transport available for 60 seconds **after it becomes idle**; it is not a maximum connection lifetime. An established transport does not reauthenticate each session after certificate rotation or expiry. Operators requiring fresh authentication must arrange to retire shared connections, coordinating with active builds. Use separate control sockets for distinct credential identities, even when they share a host/login. See the [OpenSSH connection-sharing documentation](https://man.openbsd.org/ssh_config#ControlMaster).

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

Replace `.#default` with your package attribute and `x86_64-linux` with a system supported by the gateway. The builder entry's `5` allows five simultaneous remote derivations; the following `1` is the builder's scheduling speed factor. This does not allocate five CPU cores or limit compiler parallelism inside a derivation. Choose a slot count appropriate for deployment capacity and Telchar's admission/backend limits. `--max-jobs 0` prevents local builds but still allows the client to fetch cached outputs. For persistent builder configuration, see the [Nix remote-build guide](https://nix.dev/manual/nix/stable/advanced-topics/distributed-builds).

The requesting Nix client walks the dependency graph and copies returned outputs into its own store. A fresh client can therefore issue many small requests even when gateway results are cached. SSH connection reuse and sufficient remote slots matter for this workload. For deliberately constrained single-slot tests, Nix versions exposing `build-poll-interval` can reduce postponed-build retry waits with `--option build-poll-interval 1`; this is a client scheduler setting, not a Telchar server setting.

Keep the gateway store separate from the requesting client's store. **Execution workers must not delegate their Nix builds back to the same Telchar gateway**: a worker can otherwise join its own in-progress build and wait on itself. In particular, inspect the host daemon's remote-builder configuration when a Nomad worker mounts its Nix daemon socket.

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

Every successful CI run on `main` publishes all four archives to `ghcr.io/techcyte` with the moving `main` tag. This tag identifies the latest main-branch revision that passed the regular CI suite, not a manually approved release. No `latest` tag is published.

Releases use two manual GitHub Actions workflows. **Prepare release** increments the patch when the package is already versioned for the current month, otherwise starts the month at `YYYY.M.0`; it updates all Cargo/Nix version authorities and opens a reviewable pull request. After that pull request is merged, **Publish release** verifies the prepared revision, publishes all four archives with the exact version tag, and creates the matching GitHub Release and `vYYYY.M.PATCH` tag.

The gateway and worker images are the application runtimes. The Nix-daemon image provides an isolated gateway-store sidecar, and the optional SSH-ingress image provides restricted stock-Nix ingress. Load or publish the exact archives with your container tooling.

Official gateway, Nix-daemon, and SSH-ingress account records default to UID/GID `995:995`. `nix/packages.nix` accepts `uid` and `gid` when building images with another identity and applies them to account databases, gateway frontend authorization, image users, and writable Nix-daemon state. Keep task users, mounted-directory ownership, credential ownership, and the gateway `--frontend-uid` aligned. Mount `/etc/telchar`, `/run/telchar`, persistent import and GC-root state, and the gateway Nix daemon socket. Supply PostgreSQL, credentials, configuration, and OTLP settings through operator-owned files or environment variables. The same binary provides bounded JSON inspection through `telchar operator status`, `queue`, `build`, `backends`, `recovery`, and `config-check`; see the [operator guide](docs/operations.md#read-only-operator-cli). The worker image starts `telchar-nomad-worker` and expects the bounded Nomad allocation environment documented in [Nomad backend](docs/nomad.md).

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
