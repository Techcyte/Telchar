# Checks the names, entrypoints, commands, and buildability of OCI image outputs.
{
  pkgs,
  telcharImage,
  nomadWorkerImage,
}:
assert telcharImage.imageName == "telchar";
assert telcharImage.imageTag == "latest";
assert telcharImage.imageConfig.Entrypoint == [ "/bin/telchar" ];
assert telcharImage.imageConfig.Cmd == [ "daemon" ];
assert nomadWorkerImage.imageName == "telchar-nomad-worker";
assert nomadWorkerImage.imageTag == "latest";
assert nomadWorkerImage.imageConfig.Entrypoint == [ "/bin/telchar-nomad-worker" ];
pkgs.runCommand "telchar-oci-image-contract" { } ''
  test -f ${telcharImage}
  test -f ${nomadWorkerImage}
  touch "$out"
''
