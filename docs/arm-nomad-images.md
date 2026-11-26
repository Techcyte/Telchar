# ARM64 Nomad worker image

`nix/packages.nix` defines the Nomad worker OCI image for both architectures.
`flake.nix` exports `packages.aarch64-linux.telchar-nomad-worker-oci` alongside
the ARM worker binary. Build it on a native ARM builder:

```sh
nix build -L .#packages.aarch64-linux.telchar-nomad-worker-oci
```

The native ARM GitHub Actions release job scans and publishes the Nomad worker
image. Nomad ARM tags use
`ghcr.io/techcyte/telchar-nomad-worker:<version>-arm64` (or `main-arm64` for main
publication). Existing unsuffixed Nomad tags remain x86; no multiarch manifest
is created. Deploy using a verified immutable digest, not a guessed tag digest.

This image is the allocation-side worker, not an EC2 boot image. The ARM Nomad
EC2 host AMI definition belongs to telchar-gateway at
`nix/images/telchar-nomad-arm-ami.nix`. Host enrollment, disks, instance roles,
Spot lifecycle and scaling belong to deployment infrastructure.
