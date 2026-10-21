# Canonical-store integration commands

Some stock-client contracts instantiate `/nix/store` derivations and connect to
`/nix/var/nix/daemon-socket/socket`. Their client and gateway must share an isolated
registration domain. A private noncanonical store cannot satisfy those contracts.

From the repository root, inside the flake development environment:

```bash
bash tests/canonical-store.sh \
  cargo test --locked -p telchar --test operation_dispatch -- --test-threads=1
```

The runner requires Linux unprivileged user/mount namespaces, overlayfs, an existing
`/nix/store` and `/nix/var/nix/daemon-socket` directory, Bash, util-linux and Nix.
Run it as a nonroot account with an NSS username. It creates private mounts, overlays
the installed store without modifying its lower directories, and mounts a fresh
socket directory over the canonical daemon endpoint. Store state, configuration,
logs and temporary files live under the printed `/tmp/telchar-store.*` directory.
The command and its private daemon run with the caller's nonzero UID/GID inside a
nested user namespace, allowing PostgreSQL fixtures to initialize normally.

Only that caller is trusted by the disposable daemon. Substituters and remote
builders are empty; local jobs and cores are limited to one. This test store uses
`sandbox = false`: it qualifies protocol/registration behavior, not production
sandbox security. Host daemon configuration and trust are unchanged.

The runner stops its daemon when the command exits and preserves the fixture on
success or failure for inspection. Resource containment belongs to the invoking
test environment; the runner does not install cgroup limits. Commands must not
leave background workloads behind. The existing MicroVM root-mapped helper has a
separate execution contract and is not replaced by this nonroot command runner.
