#!/usr/bin/env bash
# Scans release OCI archives for embedded secrets and records license classifications.
set -euo pipefail

cd "$(dirname "$0")/.."

for package in \
  telchar-oci \
  telchar-nix-daemon-oci \
  telchar-ssh-ingress-oci \
  telchar-nomad-worker-oci; do
  archive=$(nix build --no-link --print-out-paths ".#$package")
  nix run nixpkgs#trivy -- image \
    --input "$archive" \
    --scanners secret \
    --exit-code 1
  nix run nixpkgs#trivy -- image \
    --input "$archive" \
    --scanners license \
    --severity UNKNOWN,HIGH,CRITICAL \
    --exit-code 0
done
