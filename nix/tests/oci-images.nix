# Checks the names, entrypoints, commands, and buildability of OCI image outputs.
{
  pkgs,
  telchar,
  telcharImage,
  nixDaemonImage,
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
    "995"
  ];
assert telcharImage.imageConfig.User == "995:995";
assert nixDaemonImage.imageName == "telchar-nix-daemon";
assert nixDaemonImage.imageTag == "latest";
assert nixDaemonImage.imageConfig.Entrypoint == [ "/bin/telchar-nix-daemon" ];
assert nixDaemonImage.imageConfig.User == "995:995";
assert sshIngressImage.imageName == "telchar-ssh-ingress";
assert sshIngressImage.imageTag == "latest";
assert sshIngressImage.imageConfig.Entrypoint == [ "/bin/telchar-ssh-ingress" ];
assert nomadWorkerImage.imageName == "telchar-nomad-worker";
assert nomadWorkerImage.imageTag == "latest";
assert nomadWorkerImage.imageConfig.Entrypoint == [ "/bin/telchar-nomad-worker" ];
pkgs.runCommand "telchar-oci-image-contract" { nativeBuildInputs = [ pkgs.gnutar ]; } ''
  test -f ${telcharImage}
  test -f ${nixDaemonImage}
  test -f ${sshIngressImage}
  test -f ${nomadWorkerImage}

  mkdir ingress
  ingress_layer="$(tar -xOf ${sshIngressImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${sshIngressImage} "$ingress_layer" | tar -xf - -C ingress
  test -L ingress/bin/telchar-ssh-ingress || { echo "SSH ingress entrypoint is missing" >&2; exit 1; }
  test -L ingress/bin/telchar-ssh-forced-command || { echo "SSH forced command is missing" >&2; exit 1; }
  grep -q '^telchar:x:995:995:' ingress/etc/passwd || { echo "SSH ingress passwd identity is missing" >&2; exit 1; }
  grep -q '^telchar:x:995:' ingress/etc/group || { echo "SSH ingress group identity is missing" >&2; exit 1; }
  grep -q '^ForceCommand /bin/telchar-ssh-forced-command$' ingress/etc/ssh/sshd_config || { echo "SSH forced command configuration is missing" >&2; exit 1; }
  grep -q '^TrustedUserCAKeys /var/lib/telchar-ssh/client-ca.pub$' ingress/etc/ssh/sshd_config || { echo "SSH client CA configuration is missing" >&2; exit 1; }
  grep -q '^ExposeAuthInfo yes$' ingress/etc/ssh/sshd_config || { echo "SSH authentication metadata configuration is missing" >&2; exit 1; }
  grep -q '^DisableForwarding yes$' ingress/etc/ssh/sshd_config || { echo "SSH forwarding restriction is missing" >&2; exit 1; }
  grep -q '^PermitTTY no$' ingress/etc/ssh/sshd_config || { echo "SSH TTY restriction is missing" >&2; exit 1; }
  grep -q '^PasswordAuthentication no$' ingress/etc/ssh/sshd_config || { echo "SSH password restriction is missing" >&2; exit 1; }

  mkdir gateway
  gateway_layer="$(tar -xOf ${telcharImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${telcharImage} "$gateway_layer" | tar -xf - -C gateway
  grep -q '^telchar:x:995:995:Telchar gateway:/var/lib/telchar:/bin/false$' gateway/etc/passwd
  grep -q '^telchar:x:995:$' gateway/etc/group

  mkdir nix-daemon
  nix_daemon_layer="$(tar -xOf ${nixDaemonImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${nixDaemonImage} "$nix_daemon_layer" | tar -xf - -C nix-daemon
  test -f nix-daemon/bin/telchar-nix-daemon || { echo "Nix daemon entrypoint is missing" >&2; exit 1; }
  test -x nix-daemon/bin/telchar-nix-daemon || { echo "Nix daemon entrypoint is not executable" >&2; exit 1; }
  grep -q '^telchar:x:995:995:Telchar Nix daemon:/var/lib/telchar:/bin/false$' nix-daemon/etc/passwd
  grep -q '^telchar:x:995:$' nix-daemon/etc/group
  grep -q '^keep-failed = true$' nix-daemon/etc/nix/nix.conf
  grep -q '^keep-log = true$' nix-daemon/etc/nix/nix.conf
  test -d nix-daemon/tmp || { echo "Nix daemon /tmp is missing" >&2; exit 1; }
  if grep -aEq 'TELCHAR_TEST_(BUILD_HELPER|EXPORT_HELPER|STORE_RETENTION)' ${telchar}/bin/telchar; then
    echo "gateway OCI image contains test adapter controls" >&2
    exit 1
  fi

  touch "$out"
''
