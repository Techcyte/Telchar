# Builds Rust packages and reproducible OCI image archives exposed by the flake.
{
  pkgs,
  craneLib,
  source,
}:
let
  version = "0.1.0";

  nix-worker-protocol = craneLib.buildPackage {
    src = source;
    pname = "nix-worker-protocol";
    inherit version;
    cargoExtraArgs = "-p nix-worker-protocol";
  };

  telchar = craneLib.buildPackage {
    src = source;
    pname = "telchar";
    inherit version;
    TELCHAR_DEFAULT_SSH_PROGRAM = "${pkgs.openssh}/bin/ssh";
    cargoExtraArgs = "-p telchar";
    nativeBuildInputs = [ pkgs.postgresql ];
    cargoTestExtraArgs = "--lib";
  };

  telchar-nomad-worker = craneLib.buildPackage {
    src = source;
    pname = "telchar-nomad-worker";
    inherit version;
    cargoExtraArgs = "-p telchar-nomad-worker";
  };

  telchar-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar";
    tag = "latest";
    contents = [
      telchar
      pkgs.cacert
      pkgs.openssh
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [ "daemon" ];
    };
    config = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [ "daemon" ];
      Env = [
        "PATH=/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/tmustier/telchar";
        "org.opencontainers.image.title" = "Telchar";
      };
    };
  };

  telchar-nomad-worker-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-nomad-worker";
    tag = "latest";
    contents = [
      telchar-nomad-worker
      pkgs.cacert
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar-nomad-worker" ];
    };
    config = {
      Entrypoint = [ "/bin/telchar-nomad-worker" ];
      Env = [
        "PATH=/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/tmustier/telchar";
        "org.opencontainers.image.title" = "Telchar Nomad worker";
      };
    };
  };
in
{
  inherit
    nix-worker-protocol
    telchar
    telchar-nomad-worker
    telchar-oci
    telchar-nomad-worker-oci
    ;
  nix-reference = pkgs.nix;
  default = telchar;
}
