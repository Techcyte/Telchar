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
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "nomad";
      };
      craneLib = crane.mkLib pkgs;
      source = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter =
          path: type:
          let
            pathString = toString path;
            sshDeployment = "${toString ./.}/deploy/ssh";
          in
          craneLib.filterCargoSources path type
          || pkgs.lib.hasPrefix "${toString ./.}/crates/telchar/migrations/" pathString
          || pathString == "${toString ./.}/deploy"
          || pathString == sshDeployment
          || pkgs.lib.hasPrefix "${sshDeployment}/" pathString
          || pathString == "${toString ./.}/deny.toml"
          || pathString == "${toString ./.}/security"
          || pkgs.lib.hasPrefix "${toString ./.}/security/" pathString
          || pathString == "${toString ./.}/scripts"
          || pkgs.lib.hasPrefix "${toString ./.}/scripts/" pathString
          || pathString == "${toString ./.}/.github"
          || pkgs.lib.hasPrefix "${toString ./.}/.github/" pathString;
      };
    in
    {
      nixosModules = {
        telchar = import ./nix/nixos-module.nix;
        standalone = import ./nix/nixos-standalone.nix;
        vaultAws = import ./nix/nixos-vault-aws.nix;
        default = self.nixosModules.telchar;
      };

      lib.mkStandaloneSystem = import ./nix/mk-standalone-system.nix {
        nixosSystem = nixpkgs.lib.nixosSystem;
        standaloneModule = self.nixosModules.standalone;
      };

      packages.${system} = import ./nix/packages.nix {
        inherit pkgs craneLib source;
      };

      checks.${system} =
        import ./nix/checks/rust.nix {
          inherit pkgs craneLib source;
        }
        // import ./nix/checks/policy.nix { inherit pkgs; }
        // import ./nix/checks/nixos.nix {
          inherit pkgs system;
          nixosSystem = nixpkgs.lib.nixosSystem;
          telchar = self.packages.${system}.telchar;
          nomadWorker = self.packages.${system}.telchar-nomad-worker;
          telcharImage = self.packages.${system}.telchar-oci;
          nomadWorkerImage = self.packages.${system}.telchar-nomad-worker-oci;
          telcharModule = self.nixosModules.telchar;
          standaloneModule = self.nixosModules.standalone;
          mkStandaloneSystem = self.lib.mkStandaloneSystem;
        }
        // {
          cache-credentials = pkgs.runCommand "telchar-cache-credentials" { } ''
            export PYTHONDONTWRITEBYTECODE=1
            ${pkgs.python3}/bin/python -m unittest discover -s ${./nix}/tests -p test_cache_credentials.py
            touch $out
          '';
          oci-images = import ./nix/tests/oci-images.nix {
            inherit pkgs;
            telchar = self.packages.${system}.telchar;
            telcharImage = self.packages.${system}.telchar-oci;
            nixDaemonImage = self.packages.${system}.telchar-nix-daemon-oci;
            sshIngressImage = self.packages.${system}.telchar-ssh-ingress-oci;
            nomadWorkerImage = self.packages.${system}.telchar-nomad-worker-oci;
          };
          oci-custom-identity =
            let
              customPackages = import ./nix/packages.nix {
                inherit pkgs craneLib source;
                uid = 1234;
                gid = 1235;
              };
            in
            import ./nix/tests/oci-identity.nix {
              inherit pkgs;
              uid = 1234;
              gid = 1235;
              telcharImage = customPackages.telchar-oci;
              nixDaemonImage = customPackages.telchar-nix-daemon-oci;
              sshIngressImage = customPackages.telchar-ssh-ingress-oci;
            };
        };

      devShells.${system}.default = pkgs.mkShell {
        TELCHAR_NIX = "${pkgs.nix}/bin/nix";
        TELCHAR_NIX_BIN = "${pkgs.nix}/bin/nix";
        packages = [
          pkgs.nix
          pkgs.cargo-deny
          pkgs.trivy
          pkgs.skopeo
          pkgs.openssh
          pkgs.postgresql
          pkgs.cargo
          pkgs.clippy
          pkgs.rustc
          pkgs.rustfmt
        ];
      };
    };
}
