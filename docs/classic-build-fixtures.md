# Classic input-addressed build fixtures

## Scope and non-acceptance status

These are fixed discovery fixtures for pinned stock Nix 2.34.7, flake-locked
`NixOS/nixpkgs` revision `04607e1165ac22c5fde6dcc54c9e0b3c0487c555` and Nix
source revision `2c6d06e9387cf58167cb5a7ab91cee7333d8d17c`. They establish
only fixture input, topology, trust, and expected observation. Diagnostic
capture produced from them is explicitly **not compatibility acceptance
evidence**. No NAR body, derivation body, credential, or unbounded client
output is retained.

## Common deterministic derivation

The client evaluates this exact input-addressed derivation locally, then sends
the resulting `.drv` to the fixture-owned daemon over its Unix worker socket:

```nix
derivation {
  name = "telchar-classic-fixture";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "printf telchar-classic-fixture > \"$out\"" ];
}
```

The builder script is `printf telchar-classic-fixture > "$out"`.
Its sole output is exactly the 23 ASCII bytes `telchar-classic-fixture`.
`sha256sum` of those output bytes is
`984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f`.
The derivation is classic input-addressed: it has no `outputHash`,
`outputHashAlgo`, or `outputHashMode` attribute. Its store basename varies
with the fixture store root and is therefore asserted only as an existing path
under that fixture's `store` directory, never as a host `/nix/store` path.

The stock-client command is exactly:

```sh
nix --store unix://<fixture-socket> build --impure --expr 'derivation { name = "telchar-classic-fixture"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "printf telchar-classic-fixture > \"$out\"" ]; }' --no-link --print-out-paths
```

`<fixture-socket>` is the concrete `NixFixture` socket path. `--no-link`
prevents a result symlink outside fixture cleanup. Exact output options:
`--no-link --print-out-paths`. `--print-out-paths` is the
only accepted client output; its path must be underneath the fixture store and
its file hash must equal the stated SHA-256.

## Fixture-owned topology

One Unix user, `travis`, owns and runs each process. `NixFixture` allocates a
new temporary root. Its child paths are `store`, `state`, `log`, `config`,
`socket/daemon.sock`, and `tmp`. The daemon and client each receive all of:

```text
NIX_STORE_DIR=<fixture-root>/store
NIX_STATE_DIR=<fixture-root>/state
NIX_LOG_DIR=<fixture-root>/log
NIX_CONF_DIR=<fixture-root>/config
NIX_DAEMON_SOCKET_PATH=<fixture-root>/socket/daemon.sock
NIX_USER_CONF_FILES=/dev/null
TMPDIR=<fixture-root>/tmp
```

The exact user-configuration isolation assignment is `NIX_USER_CONF_FILES=/dev/null`.

```text
```

`NIX_CONFIG` additionally fixes `build-users-group =`, `allowed-users = *`,
`sandbox = false`, `substituters =`, and `build-hook =`. The daemon starts as
`nix-daemon`; the stock `nix` client connects only with
`--store unix://<fixture-socket>`. The system daemon and `/nix/store` are not
fixture endpoints and cannot be acceptance evidence.

Client local-build prohibition: the client never invokes a local-store build
command and no local store path is configured for the client; its sole store
argument is `--store unix://<fixture-socket>`. The daemon's isolated store is
the only store allowed to receive the derivation or execute `/bin/sh`.

Before traffic discovery, `nix --store unix://<fixture-socket> store info
--json` must report the case's stated `trusted` value. A mismatch aborts the
fixture before the build command.

Cleanup is mandatory: `NixDaemon::stop` terminates the fixture daemon, then
`NixFixture::cleanup` recursively removes the fixture root. No fixture process,
socket, state, output, or store path remains.

## Trusted fixture

The trusted fixture configures:

```text
trusted-users = travis
```

Exact configuration line: `trusted-users = travis`.

The Unix peer is `travis`, and pre-build `store info --json` must contain
`"trusted":true`. The exact build command must exit 0, report one output path
under `<fixture-root>/store`, and that output must hash to
`984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f`.

## Untrusted fixture

The untrusted fixture configures:

```text
trusted-users = root
```

Exact configuration line: `trusted-users = root`.

The same Unix peer, `travis`, is intentionally absent. Pre-build `store
info --json` must contain `"trusted":false`. The same exact build command
must exit 0, report one output path under `<fixture-root>/store`, and that
output must hash to
`984f9573538566f8f43b8333ac3ee3dfe96ea7629ffaeb4c754ac9f65ac1526f`.

Success in the untrusted case demonstrates the fixed ordinary
input-addressed build only. It does not authorize unsigned input-addressed
uploads, hidden trust escalation, content-addressed derivations, or any flow
not subsequently inventoried and typed.

## Primary Nix evidence

At source revision `2c6d06e9387cf58167cb5a7ab91cee7333d8d17c`,
`src/libstore/globals.cc` resolves `NIX_STORE_DIR`, `NIX_STATE_DIR`, and
`NIX_DAEMON_SOCKET_PATH`; `src/nix/unix/daemon.cc::authPeer` maps Unix peer
credentials against `trusted-users`; and
`src/libstore/daemon.cc::processConnection` sends the post-handshake trust
status. `src/libstore/remote-store.cc` serializes the client build request;
`src/libstore/daemon.cc::performOp` decodes and dispatches its worker
operation. Those exact serializers must be cited in the inventory before any
diagnostic candidate becomes supported observer traffic.
