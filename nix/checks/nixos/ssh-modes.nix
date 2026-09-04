# Validates independent SSH server identity and client authentication configuration.
{ pkgs, nixosSystem, telcharModule }:
let
  configuration = ingress: (nixosSystem {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [ telcharModule {
      system.stateVersion = "26.05";
      boot.loader.grub.devices = [ "/dev/vda" ];
      fileSystems."/" = { device = "/dev/vda"; fsType = "ext4"; };
      services.telchar = {
        enable = true;
        package = pkgs.hello;
        ingress.openssh = { enable = true; } // ingress;
      };
    } ];
  }).config;
  hostCertificate = configuration {
    hostCertificateFile = "/run/credentials/host-cert.pub";
  };
  clientCertificate = configuration {
    trustedUserCAKeysFile = "/run/credentials/client-ca.pub";
    authorizedPrincipalsFile = "/run/credentials/principals";
  };
  plain = configuration { };
  valid = config: builtins.all (assertion: assertion.assertion) config.assertions;
in
assert valid hostCertificate;
assert valid clientCertificate;
pkgs.runCommand "telchar-ssh-modes" { nativeBuildInputs = [ pkgs.openssh pkgs.python3 ]; } ''
  ssh-keygen -q -t ed25519 -N "" -f host-key
  ssh-keygen -q -t ed25519 -N "" -f host-ca
  ssh-keygen -q -s host-ca -h -I test -n localhost -V +1h host-key.pub
  export host_config=${hostCertificate.environment.etc."telchar/sshd_config".source}
  export client_config=${clientCertificate.environment.etc."telchar/sshd_config".source}
  export plain_config=${plain.environment.etc."telchar/sshd_config".source}
  python3 - <<'PY'
  import os
  import subprocess
  from pathlib import Path
  for name in ["host", "client", "plain"]:
      text = Path(os.environ[name + "_config"]).read_text()
      text = text.replace("/var/lib/telchar-ssh/ssh_host_ed25519_key", str(Path("host-key").resolve()))
      text = text.replace("/run/credentials/host-cert.pub", str(Path("host-key-cert.pub").resolve()))
      Path(name + ".conf").write_text(text)
      result = subprocess.run(["sshd", "-T", "-f", name + ".conf"], capture_output=True, text=True)
      assert result.returncode == 0, result.stderr
      Path(name).write_text(result.stdout)
  def settings(path):
      with open(path) as source:
          return {key.lower(): value for key, value in (line.strip().split(" ", 1) for line in source if " " in line)}
  host, client, plain = map(settings, ["host", "client", "plain"])
  assert host["trustedusercakeys"] == "none"
  assert client["trustedusercakeys"] == "/run/credentials/client-ca.pub"
  assert client["authorizedprincipalsfile"] == "/run/credentials/principals"
  assert client.get("hostcertificate", "none") == "none"
  assert plain["trustedusercakeys"] == "none"
  assert plain["authorizedkeysfile"] == "/var/lib/telchar/.ssh/authorized_keys"
  PY
  touch $out
''
