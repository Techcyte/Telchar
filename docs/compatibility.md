# Nix compatibility

Telchar's compatibility promise is deliberately narrow. A worker-protocol version match is necessary, but a client or daemon is supported only after the complete build flow has executable coverage.

## Verified client

The release suite uses stock Nix 2.34.8 and Lix 2.94.2 from the flake lock on `x86_64-linux`. Both clients exercise `ssh-ng` ingress and the local backend with classic input-addressed derivations, correct flat and recursive SHA-256 fixed-output derivations, and an incorrect-hash failure.

Supported behavior:

- stock Nix 2.34.8 `ssh-ng` ingress;
- Lix 2.94.2 `ssh-ng` ingress;
- worker-protocol negotiation through version 1.38;
- normal build mode (`0`);
- classic input-addressed and fixed-output builds through the stock-client `QueryMissing` and `BuildPathsWithResults` workflow;
- typed `BuildDerivation` requests used by Telchar's executor-facing protocol boundary;
- typed fixed-output authority across local, static SSH, and Nomad execution paths;
- exact output import and normal Nix `BuildResult` delivery.

Not supported:

- repair and check build modes;
- floating content-addressed derivations;
- Lix releases other than the exact release named above;
- Lix static SSH and Nomad backend fixtures;
- protocol flows without typed coverage and a real-client fixture.

## Gateway Nix daemon

Telchar's pure-Rust gateway-store client advertises worker protocol 1.38 and requires:

- protocol major `1`;
- daemon protocol 1.35 or later;
- the bounded operations and trust result used by Telchar.

The tested range is 1.35–1.38. Older daemons, another protocol major, malformed negotiation, and unsupported operation semantics fail closed.

## Expanding support

A compatibility addition needs:

1. an exact client or daemon version;
2. primary Nix serializer or protocol evidence;
3. typed request, response, upload, and result coverage;
4. malformed and oversized negative tests;
5. a complete real-client or real-daemon fixture;
6. release-suite coverage before the support table widens.

Lix and floating content-addressed behavior remain separate targets rather than assumed consequences of sharing protocol numbers with Nix. The Lix fixture is independent executable evidence for the local backend; it does not widen static SSH or Nomad compatibility. Static SSH and Nomad fixed-output propagation retains focused executable protocol/backend coverage with stock-Nix end-to-end evidence limited to the local backend.
