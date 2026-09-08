#!/usr/bin/env bash
# Publishes the four approved OCI image tags to GitHub Container Registry.
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

: "${GITHUB_ACTOR:?GITHUB_ACTOR is required}"
: "${GHCR_TOKEN:?GHCR_TOKEN is required}"
printf '%s' "$GHCR_TOKEN" | nix develop --command skopeo login ghcr.io --username "$GITHUB_ACTOR" --password-stdin

registry=ghcr.io/techcyte
for specification in \
  'telchar-oci:telchar' \
  'telchar-nomad-worker-oci:telchar-nomad-worker' \
  'telchar-nix-daemon-oci:telchar-nix-daemon' \
  'telchar-ssh-ingress-oci:telchar-ssh-ingress'; do
  package=${specification%%:*}
  image=${specification#*:}
  archive=$(nix build --no-link --print-out-paths ".#$package")
  nix develop --command skopeo copy \
    "docker-archive:$archive" \
    "docker://$registry/$image:$IMAGE_TAG"
done
