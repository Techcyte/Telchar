# Nix compatibility

Telchar's compatibility promise is deliberately narrow. A worker-protocol version match is necessary, but a client or daemon is supported only after the complete build flow has executable coverage.

## Verified client

The release suite uses the stock Nix and Lix packages resolved by the pinned nixpkgs revision in `flake.lock` on `x86_64-linux`. Both clients exercise `ssh-ng` ingress and the local backend with classic input-addressed derivations, correct flat and recursive SHA-256 fixed-output derivations, and an incorrect-hash failure. Exact package versions are deployment evidence from the lock, not currently asserted by the executable fixtures.

Supported behavior:

- pinned stock Nix `ssh-ng` ingress;
- pinned Lix `ssh-ng` ingress;
- worker-protocol negotiation through version 1.38;
- normal build mode (`0`);
- classic input-addressed and fixed-output builds through the stock-client `QueryMissing` and `BuildPathsWithResults` workflow;
- typed `BuildDerivation` requests used by Telchar's executor-facing protocol boundary;
- typed fixed-output authority across local, static SSH, and Nomad execution paths;
- exact output import and normal Nix `BuildResult` delivery.

Not supported:

- repair and check build modes;
- floating content-addressed derivations;
- Lix releases other than the pinned release exercised by the suite;
- Lix static SSH and Nomad backend fixtures;
- protocol flows without typed coverage and a real-client fixture.

## Executable evidence

Compatibility claims come from code and tests, not a separate support manifest.

| Claim | Focused authority | End-to-end authority |
| --- | --- | --- |
| Production worker-operation dispatch | `crates/telchar/src/service/session/mod.rs`; `crates/telchar/tests/operation_dispatch/` | stock-client VM checks below |
| Stock Nix local and fixed-output builds | local executor, operation-dispatch, store-transfer, and promotion tests | `nixos-fixed-output-local` |
| Lix local builds | typed protocol and local executor tests | `nixos-lix-local` |
| Stock Nix OCI gateway | gateway store, transfer, authentication, and session tests | `nixos-oci-gateway`; `nixos-oci-runtime` |
| OCI archive metadata and loadability | package and entrypoint contracts | `oci-images`; `nixos-oci-runtime` |
| Stock Nix static SSH | static SSH backend and worker-protocol tests | `nixos-static-ssh-gateway` |
| Stock Nix Nomad | Nomad callback, persistence, transfer, and worker tests | `nixos-nomad-gateway` |

`scripts/check-release.sh` names the end-to-end checks required for release. Reasons for tests marked `#[ignore]` remain beside the tests.

## Gateway Nix daemon

Telchar's pure-Rust gateway-store client advertises worker protocol 1.38 and requires:

- protocol major `1`;
- daemon protocol 1.35 or later;
- the bounded operations and trust result used by Telchar.

The implemented acceptance window is worker protocol 1.35–1.38. Scripted negotiation fixtures explicitly cover 1.35, 1.37, and 1.38; real-daemon integration uses the daemon supplied by the pinned nixpkgs revision. Older daemons, another protocol major, malformed negotiation, and unsupported operation semantics fail closed.

## Assigned execution

Local, static SSH, and allocation-worker build submissions send connection-scoped
`SetOptions` before `BuildDerivation`, with an empty `builders` override. Setup
failure aborts submission. Query and transfer connections retain daemon policy.
Execution options provide one local build slot, automatic core count, and enabled
substitution; they do not override sandbox or signature settings. Nix may still
start its build-hook process, which must decline when no builders are configured.

This prevents assigned builds from recursively delegating to Telchar or another
distributed builder. Ordinary requests on a worker host may still use Telchar.
It does not make routing a build back into its originating Nix store safe: the
original request can hold store locks while waiting for assigned execution that
needs those same locks. Keep that execution in a separate store domain.

`crates/telchar-nomad-worker/tests/execution_locality.rs` exercises registered
derivations against a real isolated Nix daemon, with an observable declining hook.
It covers the local executor, static SSH submission using a simulated transport
and real `nix-daemon --stdio`, and shared worker build logic using a fixture
callback. Ordinary connections before and after retain their configured builders.
This is execution-policy coverage, not OpenSSH, live Nomad, or MicroVM isolation
qualification.

## Expanding support

A compatibility addition needs:

1. an exact client or daemon version;
2. primary Nix serializer or protocol evidence;
3. typed request, response, upload, and result coverage;
4. malformed and oversized negative tests;
5. a complete real-client or real-daemon fixture;
6. release-suite coverage before the support table widens.

Lix and floating content-addressed behavior remain separate targets rather than assumed consequences of sharing protocol numbers with Nix. The Lix fixture is independent executable evidence for the local backend; it does not widen static SSH or Nomad compatibility. Static SSH and Nomad fixed-output propagation retains focused executable protocol/backend coverage with stock-Nix end-to-end evidence limited to the local backend.
