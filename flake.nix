{
  description = "Telchar Nix build distributor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { nixpkgs, crane, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      craneLib = crane.mkLib pkgs;
      source = craneLib.cleanCargoSource ./.;
    in
    {
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
        };
        default = pkgs.nix;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          pkgs.cargo
          pkgs.clippy
          pkgs.rustc
          pkgs.rustfmt
        ];
      };
    };
}
