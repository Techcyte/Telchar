# Checks the names, entrypoints, commands, and buildability of OCI image outputs.
{
  pkgs,
  telcharImage,
  nomadWorkerImage,
}:
assert telcharImage.imageName == "telchar";
assert telcharImage.imageTag == "latest";
assert telcharImage.imageConfig.Entrypoint == [ "/bin/telchar" ];
assert
  telcharImage.imageConfig.Cmd == [
    "daemon"
    "--socket"
    "/run/telchar/daemon.sock"
    "--frontend-uid"
    "0"
  ];
assert nomadWorkerImage.imageName == "telchar-nomad-worker";
assert nomadWorkerImage.imageTag == "latest";
assert nomadWorkerImage.imageConfig.Entrypoint == [ "/bin/telchar-nomad-worker" ];
pkgs.runCommand "telchar-oci-image-contract" { nativeBuildInputs = [ pkgs.gnutar ]; } ''
  test -f ${telcharImage}
  test -f ${nomadWorkerImage}

  mkdir gateway
  tar -xOf ${telcharImage} manifest.json \
    | grep -o '"[^"]*/layer.tar"' \
    | tr -d '"' \
    | while read -r layer; do
      tar -xOf ${telcharImage} "$layer" | tar -xf - -C gateway
    done
  gateway_binary="$(readlink gateway/bin/telchar)"
  gateway_binary="gateway''${gateway_binary}"
  if grep -aEq 'TELCHAR_TEST_(BUILD_HELPER|EXPORT_HELPER|STORE_RETENTION)' "$gateway_binary"; then
    echo "gateway OCI image contains test adapter controls" >&2
    exit 1
  fi

  touch "$out"
''
