# Evaluates operator-owned NixOS examples and validates rendered application settings.
{
  pkgs,
  nixosSystem,
  telcharModule,
  sshForcedCommand,
}:
let
  evaluate =
    modules:
    (nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        telcharModule
        {
          _module.args.telcharSshCommand = sshForcedCommand;
          services.telchar.package = pkgs.hello;
          boot.loader.grub.devices = [ "/dev/vda" ];
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          system.stateVersion = "26.05";
        }
      ]
      ++ modules;
    }).config;
  local = evaluate [ ../../../examples/nixos/local.nix ];
  aws = evaluate [
    ../../../examples/nixos/aws.nix
    {
      fileSystems."/var/lib/telchar" = {
        device = "/dev/disk/by-label/telchar";
        fsType = "ext4";
      };
      services.telchar.deployment = {
        nomadEndpoint = "https://nomad.example.com";
        workerImage = "registry.example.com/telchar-nomad-worker@sha256:${
          pkgs.lib.concatStrings (builtins.genList (_: "a") 64)
        }";
      };
    }
  ];
  valid = config: builtins.all (assertion: assertion.assertion) config.assertions;
in
assert valid local;
assert valid aws;
assert local.services.postgresql.enable;
assert local.services.openssh.enable;
assert !(local.systemd.services ? telchar-sshd);
assert builtins.elem "postgresql.service" local.systemd.services.telchar.requires;
assert !aws.services.postgresql.enable;
assert aws.systemd.services ? telchar-vault-aws-auth;
pkgs.runCommand "telchar-nixos-examples" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 - ${local.systemd.services.telchar.environment.TELCHAR_CONFIG} ${aws.systemd.services.telchar.environment.TELCHAR_CONFIG} <<'PY'
  import sys
  import tomllib
  with open(sys.argv[1], 'rb') as source:
      local = tomllib.load(source)
  with open(sys.argv[2], 'rb') as source:
      aws = tomllib.load(source)
  assert local['backends']['local']['maximum_concurrent_builds'] == 4
  assert aws['database']['url_file'] == '/var/lib/telchar/credentials/database-url'
  assert aws['backends']['nomad'][0]['aws-spot']['endpoint'] == 'https://nomad.example.com'
  PY
  touch $out
''
