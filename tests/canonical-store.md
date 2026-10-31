# Canonical-store integration commands

Some stock-client contracts instantiate `/nix/store` derivations and connect to
`/nix/var/nix/daemon-socket/socket`. Their client and gateway must share an isolated
registration domain. A private noncanonical store cannot satisfy those contracts.

CI and release qualification build the complete Nix-owned result:

```bash
nix build -L --no-link .#checks.x86_64-linux.workspace-tests
```

This requires a VM-capable Linux builder with KVM. Crane caches dependency
compilation independently of source changes and precompiles all workspace test
artifacts with debug assertions. The VM restores source, Cargo configuration and
target artifacts to their original absolute paths before invoking the same
package-partitioned runner offline. This preserves embedded `CARGO_BIN_EXE_*` and
`CARGO_MANIFEST_DIR` paths and keeps Cargo responsible for doctests and target
selection. The guest is a nonroot NSS user; the VM check result depends on every
package group succeeding. Workspace output streams to the VM console and is
copied as `workspace.log` to the test driver output (on failure, inspect the
retained Nix build directory or use the interactive test driver).

For focused development, from the repository root inside the flake environment:

```bash
bash tests/canonical-store.sh \
  cargo test --locked -p telchar --test operation_dispatch -- --test-threads=1
```

For all workspace packages, use the package-partitioned runner:

```bash
cargo build --locked -p telchar --bin telchar
export TELCHAR_TEST_DAEMON_BIN="$PWD/target/debug/telchar"
bash tests/workspace.sh --locked
```

Arguments to `tests/workspace.sh` are Cargo options before the test separator.
It runs every workspace package except `telchar-nomad-worker` inside the canonical
runner, then runs the complete worker package outside that additional overlay.
Worker contracts create their own gateway and worker stores; wrapping them in a
third overlay exceeds Linux filesystem stacking depth. Both groups run serially,
with no test exclusions within either package group. Do not wrap the workspace
runner itself in `tests/canonical-store.sh`. Release-profile qualification needs
a daemon built with the existing test endpoint seam enabled; production release
packages must not enable that seam.

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
