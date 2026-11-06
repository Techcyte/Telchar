#!/usr/bin/env bash
# Publishes versioned OCI images for the selected architecture to GitHub Container Registry.
set -euo pipefail

cd "$(dirname "$0")/.."

: "${IMAGE_TAG:?IMAGE_TAG is required}"
if [[ $IMAGE_TAG == main ]]; then
  :
elif [[ $IMAGE_TAG =~ ^[1-9][0-9]{3}\.([1-9]|1[0-2])\.[0-9]+$ ]]; then
  manifest_version=$(nix eval --raw .#packages.x86_64-linux.telchar.version)
  if [[ $IMAGE_TAG != "$manifest_version" ]]; then
    echo "image tag $IMAGE_TAG does not match package version $manifest_version" >&2
    exit 1
  fi
else
  echo "image tag must be main or match YYYY.M.PATCH" >&2
  exit 1
fi

system=${IMAGE_SYSTEM:-x86_64-linux}
case "$system" in
  x86_64-linux)
    specifications=(
      'telchar-oci:telchar'
      'telchar-nomad-worker-oci:telchar-nomad-worker'
      'telchar-nix-daemon-oci:telchar-nix-daemon'
      'telchar-ssh-ingress-oci:telchar-ssh-ingress'
    )
    ;;
  aarch64-linux)
    specifications=(
      'telchar-microvm-execution-oci:telchar-microvm-execution'
      'telchar-nomad-worker-oci:telchar-nomad-worker'
    )
    ;;
  *)
    echo "unsupported image system: $system" >&2
    exit 1
    ;;
esac

: "${GITHUB_ACTOR:?GITHUB_ACTOR is required}"
: "${GHCR_TOKEN:?GHCR_TOKEN is required}"
printf '%s' "$GHCR_TOKEN" | nix develop --command skopeo login ghcr.io --username "$GITHUB_ACTOR" --password-stdin

registry=ghcr.io/techcyte
for specification in "${specifications[@]}"; do
  package=${specification%%:*}
  image=${specification#*:}
  destination_tag=$IMAGE_TAG
  # Keep existing x86 tags intact; no last-writer-wins architecture overwrite.
  if [[ $system == aarch64-linux && $image == telchar-nomad-worker ]]; then
    destination_tag+=-arm64
  fi
  archive=$(nix build --no-link --print-out-paths ".#packages.$system.$package")
  nix develop --command skopeo copy \
    "docker-archive:$archive" \
    "docker://$registry/$image:$destination_tag"
done
