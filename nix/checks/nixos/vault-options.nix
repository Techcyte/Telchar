# Evaluates independent Vault credential capabilities without unrelated consumers.
{
  pkgs,
  nixosSystem,
  telcharModule,
}:
let
  configuration =
    provider: settings:
    (nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        telcharModule
        ../../nixos-vault-aws.nix
        {
          system.stateVersion = "26.05";
          boot.loader.grub.devices = [ "/dev/vda" ];
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          services.telchar = {
            enable = true;
            package = pkgs.hello;
            vaultAwsAuth = {
              enable = true;
              address = "https://vault.example.invalid";
              role = "gateway";
              renewal.enable = true;
            }
            // provider;
          }
          // settings;
        }
      ];
    }).config;
  ssh = configuration { sshSignPath = "ssh/sign/host"; } {
    ingress.openssh.hostCertificateFile = "/run/credentials/host-cert.pub";
    sshHostCertificateRenewal = {
      enable = true;
      candidateFile = "/run/credentials/host-candidate";
      expectedSigningCAFile = "/run/credentials/host-ca";
      expectedPrincipals = [ "gateway" ];
    };
  };
  nomad = configuration { nomadSecretPath = "nomad/creds/gateway"; } {
    nomad = {
      tokenFile = "/run/credentials/nomad";
      renewal.enable = true;
      renewal.candidateFile = "/run/credentials/nomad-candidate";
    };
  };
  valid = config: builtins.all (assertion: assertion.assertion) config.assertions;
in
assert valid ssh;
assert valid nomad;
assert !(ssh.systemd.services ? telchar-nomad-credential-renewal);
assert !(nomad.systemd.services ? telchar-ssh-host-certificate-renewal);
pkgs.runCommand "telchar-vault-options" { } "touch $out"
