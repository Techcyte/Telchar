# Builds Rust packages and reproducible OCI image archives exposed by the flake.
{
  pkgs,
  craneLib,
  source,
  uid ? 995,
  gid ? uid,
}:
let
  version = "2026.10.1";
  uidString = toString uid;
  gidString = toString gid;
  buildArgs = {
    src = source;
    inherit version;
    TELCHAR_DEFAULT_SSH_PROGRAM = "${pkgs.openssh}/bin/ssh";
    nativeBuildInputs = [
      pkgs.postgresql
      pkgs.nix
      pkgs.openssh
    ];
  };
  cargoArtifacts = craneLib.buildDepsOnly (
    buildArgs
    // {
      pname = "telchar";
      cargoExtraArgs = "--locked -p telchar -p telchar-nomad-worker";
    }
  );

  nix-worker-protocol = craneLib.buildPackage {
    src = source;
    pname = "nix-worker-protocol";
    inherit version;
    cargoExtraArgs = "-p nix-worker-protocol";
  };

  telchar = craneLib.buildPackage (
    buildArgs
    // {
      pname = "telchar";
      inherit cargoArtifacts;
      cargoExtraArgs = "--locked -p telchar";
      cargoTestExtraArgs = "--lib";
    }
  );

  telchar-nomad-worker =
    (craneLib.buildPackage (
      buildArgs
      // {
        pname = "telchar-nomad-worker";
        inherit cargoArtifacts;
        nativeCheckInputs = [
          pkgs.cacert
          pkgs.python3
        ]
        ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
          pkgs.util-linux
          pkgs.bubblewrap
        ];
        cargoTestCommand =
          if pkgs.stdenv.hostPlatform.isLinux then
            "bash scripts/test-worker-package.sh \"$workerTestStore\" bash -c 'cargoWithProfile test \"$@\"' bash"
          else
            "cargoWithProfile test";
        cargoExtraArgs = "--locked -p telchar-nomad-worker";
        preCheck = ''
          export TELCHAR_NIX_BIN=${pkgs.nix}/bin/nix
          daemonBuildLog=$(mktemp)
          cargoWithProfile build --locked -p telchar --bin telchar \
            --config "profile.''${CARGO_PROFILE:-dev}.package.telchar.debug-assertions=true" \
            --message-format json-render-diagnostics > "$daemonBuildLog"
          daemonExecutable=$(${pkgs.jq}/bin/jq -rs \
            '[.[] | select(.reason == "compiler-artifact" and .target.name == "telchar" and .executable != null)] | last | .executable' \
            "$daemonBuildLog")
          test -x "$daemonExecutable"
          export TELCHAR_TEST_DAEMON_BIN="$TMPDIR/telchar-test-daemon"
          cp "$daemonExecutable" "$TELCHAR_TEST_DAEMON_BIN"
        ''
        + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
          export -f cargo cargoWithProfile
          export workerTestStore="$TMPDIR/worker-test-store"
          mkdir "$workerTestStore"
          ${pkgs.python3}/bin/python3 scripts/test-worker-store.py
          bash scripts/prepare-worker-store.sh "$workerTestInputs/store-paths" "$workerTestStore"
          chmod u+w "$workerTestStore"
          checkCpus=$(${pkgs.python3}/bin/python3 -c 'import os; print(",".join(map(str, sorted(os.sched_getaffinity(0))[:2])))')
          taskset -pc "$checkCpus" $$
        '';
        postInstall = ''
          test ! -e "$out/bin/telchar"
          test ! -e "$out/bin/telchar-test-daemon"
        '';
      }
    )).overrideAttrs
      (previous: {
        workerTestInputs = pkgs.closureInfo {
          rootPaths = [
            pkgs.stdenv
            source
            previous.cargoArtifacts
            previous.cargoVendorDir
          ]
          ++ previous.nativeBuildInputs
          ++ previous.nativeCheckInputs
          ++ (previous.buildInputs or [ ]);
        };
      });

  sshIngressEntrypoint = pkgs.writeShellScriptBin "telchar-ssh-ingress" (
    builtins.readFile ../deploy/ssh/telchar-ssh-ingress.sh
  );
  sshIngressForcedCommand = pkgs.writeShellScriptBin "telchar-ssh-forced-command" (
    builtins.readFile ../deploy/ssh/telchar-ssh-forced-command.sh
  );
  sshIngressEtc = pkgs.runCommand "telchar-ssh-ingress-etc" { } ''
        mkdir -p "$out/etc/ssh" "$out/var/empty" "$out/tmp"
        chmod 1777 "$out/tmp"
        cp ${../deploy/ssh/sshd_config} "$out/etc/ssh/sshd_config"
        cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    sshd:x:994:994:OpenSSH privilege separation:/var/empty:/bin/false
    telchar:x:${uidString}:${gidString}:Telchar SSH ingress:/var/empty:/bin/bash
    EOF
        cat > "$out/etc/group" <<'EOF'
    root:x:0:
    sshd:x:994:
    telchar:x:${gidString}:
    EOF
  '';

  gatewayEtc = pkgs.runCommand "telchar-gateway-etc" { } ''
    mkdir -p "$out/etc"
    cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    telchar:x:${uidString}:${gidString}:Telchar gateway:/var/lib/telchar:/bin/false
    EOF
    cat > "$out/etc/group" <<'EOF'
    root:x:0:
    telchar:x:${gidString}:
    EOF
  '';

  nixDaemonClosure = pkgs.closureInfo {
    rootPaths = [
      pkgs.nix
      pkgs.cacert
    ];
  };
  nixDaemonBootstrap =
    pkgs.runCommand "telchar-nix-daemon-bootstrap" { nativeBuildInputs = [ pkgs.gnutar ]; }
      ''
        mkdir -p "$out"
        tar --mode=u+w -cf "$out/store.tar" --files-from=${nixDaemonClosure}/store-paths
        cp ${nixDaemonClosure}/registration "$out/registration"
      '';
  nixDaemonEntrypoint = pkgs.writeText "telchar-nix-daemon" (
    builtins.replaceStrings [ "@cacert@" "@nix@" ] [ "${pkgs.cacert}" "${pkgs.nix}" ] (
      builtins.readFile ../deploy/nix/telchar-nix-daemon.sh
    )
  );
  nixDaemonEtc = pkgs.runCommand "telchar-nix-daemon-etc" { } ''
    mkdir -p "$out/etc/nix"
    cat > "$out/etc/passwd" <<'EOF'
    root:x:0:0:root:/root:/bin/bash
    telchar:x:${uidString}:${gidString}:Telchar Nix daemon:/var/lib/telchar:/bin/false
    EOF
    cat > "$out/etc/group" <<'EOF'
    root:x:0:
    telchar:x:${gidString}:
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
    tag = version;
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
        uidString
      ];
      User = "${uidString}:${gidString}";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar";
      };
    };
    config = {
      Entrypoint = [ "/bin/telchar" ];
      Cmd = [
        "daemon"
        "--socket"
        "/run/telchar/daemon.sock"
        "--frontend-uid"
        uidString
      ];
      Env = [
        "HOME=/var/lib/telchar"
        "PATH=/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      User = "${uidString}:${gidString}";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar";
      };
    };
  };

  telchar-nix-daemon-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-nix-daemon";
    tag = version;
    fakeRootCommands = ''
      mkdir -p ./bootstrap ./bin ./etc/nix ./nix/var/log/nix/drvs ./tmp ./var/lib/telchar
      chown -R ${uidString}:${gidString} ./nix/var/log ./var/lib/telchar
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
      Env = [
        "HOME=/var/lib/telchar"
        "NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "PATH=/bin"
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      ];
      User = "${uidString}:${gidString}";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar Nix daemon";
      };
    };
    config = {
      Entrypoint = [ "/bin/telchar-nix-daemon" ];
      Env = [
        "HOME=/var/lib/telchar"
        "NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "PATH=/bin"
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      ];
      User = "${uidString}:${gidString}";
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar Nix daemon";
      };
    };
  };

  telchar-ssh-ingress-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-ssh-ingress";
    tag = version;
    fakeRootCommands = ''
      rm -rf ./etc/ssh ./var/empty
      cp -R ${sshIngressEtc}/etc/. ./etc/
      cp -R ${sshIngressEtc}/var/. ./var/
      cp -R ${sshIngressEtc}/tmp ./tmp
      chmod 1777 ./tmp
    '';
    contents = [
      telchar
      sshIngressEntrypoint
      sshIngressForcedCommand
      pkgs.bash
      pkgs.coreutils
      pkgs.gawk
      pkgs.openssh
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar-ssh-ingress" ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar SSH ingress";
      };
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
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar SSH ingress";
      };
    };
  };

  telchar-nomad-worker-oci = pkgs.dockerTools.buildLayeredImage {
    name = "telchar-nomad-worker";
    tag = version;
    contents = [
      telchar-nomad-worker
      pkgs.cacert
    ];
    passthru.imageConfig = {
      Entrypoint = [ "/bin/telchar-nomad-worker" ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
        "org.opencontainers.image.title" = "Telchar Nomad worker";
      };
    };
    config = {
      Entrypoint = [ "/bin/telchar-nomad-worker" ];
      Env = [
        "PATH=/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      ];
      Labels = {
        "org.opencontainers.image.source" = "https://github.com/techcyte/telchar";
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
