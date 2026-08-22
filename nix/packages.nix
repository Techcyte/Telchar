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

  gatewayEtc = pkgs.runCommand "telchar-gateway-etc" { } ''
    mkdir -p "$out/etc"
    cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    telchar:x:995:995:Telchar gateway:/var/lib/telchar:/bin/false
    EOF
    cat > "$out/etc/group" <<'EOF'
    root:x:0:
    telchar:x:995:
    EOF
  '';

  nixDaemonClosure = pkgs.closureInfo {
    rootPaths = [ pkgs.nix ];
  };
  nixDaemonBootstrap =
    pkgs.runCommand "telchar-nix-daemon-bootstrap" { nativeBuildInputs = [ pkgs.gnutar ]; }
      ''
        mkdir -p "$out"
        tar --mode=u+w -cf "$out/store.tar" --files-from=${nixDaemonClosure}/store-paths
        cp ${nixDaemonClosure}/registration "$out/registration"
      '';
  nixDaemonEntrypoint = pkgs.writeText "telchar-nix-daemon" (
    builtins.replaceStrings [ "@nix@" ] [ "${pkgs.nix}" ] (
      builtins.readFile ../deploy/nix/telchar-nix-daemon.sh
    )
  );
  nixDaemonEtc = pkgs.runCommand "telchar-nix-daemon-etc" { } ''
    mkdir -p "$out/etc/nix"
    cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    telchar:x:995:995:Telchar Nix daemon:/var/lib/telchar:/bin/false
    EOF
    cat > "$out/etc/group" <<'EOF'
    root:x:0:
    telchar:x:995:
    EOF
    cat > "$out/etc/nix/nix.conf" <<'EOF'
    build-users-group =
    sandbox = false
    keep-failed = true
    keep-build-log = true
    EOF
  '';

  telchar-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar";
    tag = "latest";
    fakeRootCommands = ''
      cp ${gatewayEtc}/etc/passwd ./etc/passwd
      cp ${gatewayEtc}/etc/group ./etc/group
    '';
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
        "995"
      ];
      User = "995:995";
    };
    config = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [
        "daemon"
        "--socket"
        "/run/telchar/daemon.sock"
        "--frontend-uid"
        "995"
      ];
      Env = [
        "HOME=/var/lib/telchar"
        "PATH=/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      User = "995:995";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/tmustier/telchar";
        "org.opencontainers.image.title" = "Telchar";
      };
    };
  };

  telchar-nix-daemon-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-nix-daemon";
    tag = "latest";
    fakeRootCommands = ''
      mkdir -p ./bootstrap ./bin ./etc/nix ./nix/var/log/nix/drvs ./tmp ./var/lib/telchar
      chown -R 995:995 ./nix/var/log ./var/lib/telchar
      cp ${pkgs.pkgsStatic.busybox}/bin/busybox ./bootstrap/busybox
      cp ${nixDaemonBootstrap}/store.tar ./bootstrap/store.tar
      cp ${nixDaemonBootstrap}/registration ./bootstrap/registration
      cp ${nixDaemonEntrypoint} ./bin/telchar-nix-daemon
      chmod 0555 ./bootstrap/busybox ./bin/telchar-nix-daemon
      chmod 1777 ./tmp
      cp ${nixDaemonEtc}/etc/passwd ./etc/passwd
      cp ${nixDaemonEtc}/etc/group ./etc/group
      cp ${nixDaemonEtc}/etc/nix/nix.conf ./etc/nix/nix.conf
    '';
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar-nix-daemon" ];
      User = "995:995";
    };
    config = {
      Entrypoint = [ "/bin/telchar-nix-daemon" ];
      Env = [
        "HOME=/var/lib/telchar"
        "PATH=/bin"
      ];
      User = "995:995";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/tmustier/telchar";
        "org.opencontainers.image.title" = "Telchar Nix daemon";
      };
    };
  };

  telchar-ssh-ingress-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-ssh-ingress";
    tag = "latest";
    fakeRootCommands = ''
      rm -rf ./etc/ssh ./var/empty
      cp -R ${sshIngressEtc}/etc/. ./etc/
      cp -R ${sshIngressEtc}/var/. ./var/
    '';
    contents = [
      telchar
      sshIngressEntrypoint
      sshIngressForcedCommand
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
    telchar-nix-daemon-oci
    telchar-ssh-ingress-oci
    telchar-nomad-worker-oci
    ;
  nix-reference = pkgs.nix;
  default = telchar;
}
