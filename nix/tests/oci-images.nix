# Checks the names, entrypoints, commands, and buildability of OCI image outputs.
{
  pkgs,
  telcharImage,
  sshIngressImage,
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
assert sshIngressImage.imageName == "telchar-ssh-ingress";
assert sshIngressImage.imageTag == "latest";
assert sshIngressImage.imageConfig.Entrypoint == [ "/bin/telchar-ssh-ingress" ];
assert nomadWorkerImage.imageName == "telchar-nomad-worker";
assert nomadWorkerImage.imageTag == "latest";
assert nomadWorkerImage.imageConfig.Entrypoint == [ "/bin/telchar-nomad-worker" ];
pkgs.runCommand "telchar-oci-image-contract" { nativeBuildInputs = [ pkgs.gnutar ]; } ''
  test -f ${telcharImage}
  test -f ${sshIngressImage}
  test -f ${nomadWorkerImage}

  mkdir ingress
  tar -xOf ${sshIngressImage} manifest.json \
    | grep -o '"[^"]*/layer.tar"' \
    | tr -d '"' \
    | while read -r layer; do
      tar -xOf ${sshIngressImage} "$layer" | tar -xf - -C ingress
    done
  test -x ingress/bin/telchar-ssh-ingress
  test -x ingress/bin/telchar-ssh-forced-command
  grep -q '^telchar:x:995:995:' ingress/etc/passwd
  grep -q '^telchar:x:995:' ingress/etc/group
  grep -q '^ForceCommand /bin/telchar-ssh-forced-command$' ingress/etc/ssh/sshd_config
  grep -q '^TrustedUserCAKeys /var/lib/telchar-ssh/client-ca.pub$' ingress/etc/ssh/sshd_config
  grep -q '^ExposeAuthInfo yes$' ingress/etc/ssh/sshd_config
  grep -q '^DisableForwarding yes$' ingress/etc/ssh/sshd_config
  grep -q '^PermitTTY no$' ingress/etc/ssh/sshd_config
  grep -q '^PasswordAuthentication no$' ingress/etc/ssh/sshd_config

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
