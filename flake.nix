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
      source = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter =
          path: type:
          craneLib.filterCargoSources path type
          || pkgs.lib.hasPrefix "${toString ./.}/crates/telchar/migrations/" (toString path);
      };
    in
    {
      nixosModules = {
        telchar = import ./nix/nixos-module.nix;
        default = self.nixosModules.telchar;
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
          telchar = self.packages.${system}.telchar;
          nomadWorker = self.packages.${system}.telchar-nomad-worker;
          telcharModule = self.nixosModules.telchar;
        }
        // {
          oci-images = import ./nix/tests/oci-images.nix {
            inherit pkgs;
            telcharImage = self.packages.${system}.telchar-oci;
            nomadWorkerImage = self.packages.${system}.telchar-nomad-worker-oci;
          };
        };

      devShells.${system}.default = pkgs.mkShell {
        TELCHAR_NIX = "${pkgs.nix}/bin/nix";
        TELCHAR_NIX_BIN = "${pkgs.nix}/bin/nix";
        packages = [
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
