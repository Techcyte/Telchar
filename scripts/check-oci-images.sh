#!/usr/bin/env bash
# Scans release OCI archives for embedded secrets and records license classifications.
set -euo pipefail

cd "$(dirname "$0")/.."

system=${IMAGE_SYSTEM:-x86_64-linux}
case "$system" in
  x86_64-linux)
    packages=(telchar-oci telchar-nix-daemon-oci telchar-ssh-ingress-oci telchar-nomad-worker-oci)
    ;;
  aarch64-linux)
    packages=(telchar-microvm-execution-oci)
    ;;
  *)
    echo "unsupported image system: $system" >&2
    exit 1
    ;;
esac

for package in "${packages[@]}"; do
  archive=$(nix build --no-link --print-out-paths ".#packages.$system.$package")
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
