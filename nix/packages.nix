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

  sshIngressEntrypoint = pkgs.writeShellScriptBin "telchar-ssh-ingress" (
    builtins.readFile ../deploy/ssh/telchar-ssh-ingress.sh
  );
  sshIngressForcedCommand = pkgs.writeShellScriptBin "telchar-ssh-forced-command" (
    builtins.readFile ../deploy/ssh/telchar-ssh-forced-command.sh
  );
  sshIngressEtc = pkgs.runCommand "telchar-ssh-ingress-etc" { } ''
        mkdir -p "$out/etc/ssh" "$out/var/empty"
        cp ${../deploy/ssh/sshd_config} "$out/etc/ssh/sshd_config"
        cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    telchar:x:995:995:Telchar SSH ingress:/var/empty:/bin/false
    EOF
        cat > "$out/etc/group" <<'EOF'
    root:x:0:
    telchar:x:995:
    EOF
  '';

  telchar-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar";
    tag = "latest";
    contents = [
      telchar
      pkgs.cacert
      pkgs.openssh
      pkgs.bash
      pkgs.nix
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [
        "daemon"
        "--socket"
        "/run/telchar/daemon.sock"
        "--frontend-uid"
        "0"
      ];
    };
    config = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [
        "daemon"
        "--socket"
        "/run/telchar/daemon.sock"
        "--frontend-uid"
        "0"
      ];
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

  telchar-ssh-ingress-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-ssh-ingress";
    tag = "latest";
    contents = [
      telchar
      sshIngressEntrypoint
      sshIngressForcedCommand
      sshIngressEtc
      pkgs.bash
      pkgs.cacert
      pkgs.coreutils
      pkgs.curl
      pkgs.gawk
      pkgs.gnugrep
      pkgs.jq
      pkgs.openssh
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar-ssh-ingress" ];
    };
    config = {
      Entrypoint = [ "/bin/telchar-ssh-ingress" ];
      Env = [
        "PATH=/bin:/usr/bin:/usr/sbin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      User = "0:0";
      ExposedPorts = {
        "2222/tcp" = { };
      };
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/tmustier/telchar";
        "org.opencontainers.image.title" = "Telchar SSH ingress";
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
    telchar-ssh-ingress-oci
    telchar-nomad-worker-oci
    ;
  nix-reference = pkgs.nix;
  default = telchar;
}
