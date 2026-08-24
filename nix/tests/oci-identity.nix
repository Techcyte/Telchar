# Verifies configurable OCI account identity in image metadata and account databases.
{
  pkgs,
  uid,
  gid,
  telcharImage,
  nixDaemonImage,
  sshIngressImage,
}:
assert telcharImage.imageConfig.User == "${toString uid}:${toString gid}";
assert nixDaemonImage.imageConfig.User == "${toString uid}:${toString gid}";
pkgs.runCommand "telchar-oci-identity-${toString uid}-${toString gid}" { nativeBuildInputs = [ pkgs.gnutar ]; } ''
  mkdir gateway nix-daemon ingress
  gateway_layer="$(tar -xOf ${telcharImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${telcharImage} "$gateway_layer" | tar -xf - -C gateway
  grep -q '^telchar:x:${toString uid}:${toString gid}:Telchar gateway:' gateway/etc/passwd
  grep -q '^telchar:x:${toString gid}:$' gateway/etc/group

  nix_daemon_layer="$(tar -xOf ${nixDaemonImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${nixDaemonImage} "$nix_daemon_layer" | tar -xf - -C nix-daemon
  grep -q '^telchar:x:${toString uid}:${toString gid}:Telchar Nix daemon:' nix-daemon/etc/passwd
  grep -q '^telchar:x:${toString gid}:$' nix-daemon/etc/group

  ingress_layer="$(tar -xOf ${sshIngressImage} manifest.json | grep -o '"[^"]*/layer.tar"' | tr -d '"' | tail -n 1)"
  tar -xOf ${sshIngressImage} "$ingress_layer" | tar -xf - -C ingress
  grep -q '^telchar:x:${toString uid}:${toString gid}:Telchar SSH ingress:' ingress/etc/passwd
  grep -q '^telchar:x:${toString gid}:$' ingress/etc/group
  touch "$out"
''
