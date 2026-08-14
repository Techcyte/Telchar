# Nix compatibility

Telchar's compatibility promise is deliberately narrow. A worker-protocol version match is necessary, but a client or daemon is supported only after the complete build flow has executable coverage.

## Verified client

The release suite uses stock Nix from the flake lock on `x86_64-linux` and exercises trusted and untrusted `ssh-ng` sessions with classic input-addressed derivations. Stock-Nix local-backend fixtures additionally cover correct flat and recursive SHA-256 fixed-output derivations and an incorrect-hash failure.

Supported behavior:

- stock Nix `ssh-ng` ingress;
- worker-protocol negotiation through version 1.38;
- normal build mode (`0`);
- classic input-addressed and fixed-output `BuildDerivation` requests;
- typed fixed-output authority across local, static SSH, and Nomad execution paths;
- exact output import and normal Nix `BuildResult` delivery.

Not supported:

- repair and check build modes;
- floating content-addressed derivations;
- Lix clients;
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

Lix and floating content-addressed behavior are separate targets rather than assumed consequences of sharing protocol numbers with Nix. Static SSH and Nomad fixed-output propagation has focused executable protocol/backend coverage; the stock-Nix fixed-output VM fixture currently exercises the local backend.
