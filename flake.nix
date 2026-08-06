{
  description = "Telchar Nix build distributor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      ...
    }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      craneLib = crane.mkLib pkgs;
      source = craneLib.cleanCargoSource ./.;
    in
    {
      checks.${system} =
        let
          cargoArtifacts = craneLib.buildDepsOnly {
            src = source;
            pname = "telchar";
            version = "0.1.0";
          };
        in
        {
          format = craneLib.cargoFmt {
            src = source;
            pname = "telchar";
            version = "0.1.0";
          };
          lint = craneLib.cargoClippy {
            src = source;
            pname = "telchar";
            version = "0.1.0";
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets --all-features -- -D warnings";
          };
          unit-tests = craneLib.cargoTest {
            src = source;
            pname = "telchar";
            version = "0.1.0";
            inherit cargoArtifacts;
            cargoTestExtraArgs = "--lib";
          };
          nixos-test-library =
            let
              harness = import ./tests/nixos/lib.nix {
                inherit pkgs;
                telchar = self.packages.${system}.telchar;
              };
            in
            harness.mkTest {
              name = "telchar-nixos-test-library";
              testScript = ''
                start_all()
              '';
            };
          nixos-smoke =
            let
              harness = import ./tests/nixos/lib.nix {
                inherit pkgs;
                telchar = self.packages.${system}.telchar;
              };
            in
            harness.mkTest {
              name = "telchar-nixos-smoke";
              includeCollector = true;
              testScript = ''
                start_all()
                otlp_collector.wait_for_open_port(4317)
                gateway.succeed("systemctl restart telchar.service")
                gateway.wait_for_unit("telchar.service")
                stock_client.succeed("ping -c 1 gateway")
                otlp_collector.succeed("test -s /var/lib/telchar-otlp/records.json")
              '';
            };
          nixos-artifacts =
            let
              harness = import ./tests/nixos/lib.nix {
                inherit pkgs;
                telchar = self.packages.${system}.telchar;
              };
            in
            harness.mkTest {
              name = "telchar-nixos-artifacts";
              includeCollector = true;
              testScript = ''
                start_all()
                otlp_collector.wait_for_open_port(4317)
                gateway.succeed("systemctl restart telchar.service")
                gateway.wait_for_unit("telchar.service")
                gateway.fail("systemctl start telchar-artifacts-failure.service")
                gateway.succeed("systemctl start telchar-artifacts.service")
                gateway.wait_for_unit("telchar-artifacts.service")
                gateway.succeed("test -s /var/lib/telchar-artifacts/journal.log")
                gateway.succeed("test -s /var/lib/telchar-artifacts/machine-state.json")
                gateway.succeed("grep -q telchar-artifacts-failure /var/lib/telchar-artifacts/journal.log")
                gateway.succeed("grep -q ActiveState=failed /var/lib/telchar-artifacts/machine-state.json")
                gateway.succeed("! grep -q not-for-artifacts /var/lib/telchar-artifacts/journal.log /var/lib/telchar-artifacts/machine-state.json")
                otlp_collector.succeed("mkdir -p /var/lib/telchar-artifacts && cp /var/lib/telchar-otlp/records.json /var/lib/telchar-artifacts/otlp-records.json")
                otlp_collector.succeed("test -s /var/lib/telchar-artifacts/otlp-records.json")
              '';
            };
        };

      packages.${system} = {
        nix-worker-protocol = craneLib.buildPackage {
          src = source;
          pname = "nix-worker-protocol";
          version = "0.1.0";
          cargoExtraArgs = "-p nix-worker-protocol";
        };
        telchar = craneLib.buildPackage {
          src = source;
          pname = "telchar";
          version = "0.1.0";
          cargoExtraArgs = "-p telchar";
          cargoTestExtraArgs = "--lib";
        };
        default = pkgs.nix;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          pkgs.openssh
          pkgs.cargo
          pkgs.clippy
          pkgs.rustc
          pkgs.rustfmt
        ];
      };
    };
}
