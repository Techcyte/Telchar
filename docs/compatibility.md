# Nix compatibility

Telchar's compatibility promise is deliberately narrow. A worker-protocol version match is necessary, but a client or daemon is supported only after the complete build flow has executable coverage.

## Verified client

The release suite uses stock Nix from the flake lock on `x86_64-linux` and exercises trusted and untrusted `ssh-ng` sessions with classic input-addressed derivations.

Supported behavior:

- stock Nix `ssh-ng` ingress;
- worker-protocol negotiation through version 1.38;
- normal build mode (`0`);
- classic input-addressed `BuildDerivation` requests;
- local, static SSH, and Nomad execution;
- exact output import and normal Nix `BuildResult` delivery.

Not supported:

- repair and check build modes;
- fixed-output derivations;
- content-addressed derivations;
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

Lix and content-addressed behavior are separate targets rather than assumed consequences of sharing protocol numbers with Nix.
