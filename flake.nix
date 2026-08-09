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
      mkNixStoreHelper =
        name: sourceDirectory:
        pkgs.stdenv.mkDerivation {
          pname = "telchar-nix-store-${name}";
          version = "0.1.0";
          src = sourceDirectory;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.nix.dev ];
          buildPhase = ''
            runHook preBuild
            $CXX $(pkg-config --cflags nix-store) -o telchar-nix-store-${name} main.cc $(pkg-config --libs nix-store)
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            install -Dm755 telchar-nix-store-${name} $out/libexec/telchar/nix-store-${name}
            runHook postInstall
          '';
        };
      nixStoreBuild = mkNixStoreHelper "build" ./tools/nix-store-build;
      nixStoreExport = mkNixStoreHelper "export" ./tools/nix-store-export;
      nixStorePromote = mkNixStoreHelper "promote" ./tools/nix-store-promote;
      nixStoreClosure = mkNixStoreHelper "closure" ./tools/nix-store-closure;
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
            nativeBuildInputs = [ pkgs.postgresql ];
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
                otlp_collector.wait_until_succeeds("test -s /var/lib/telchar-otlp/records.json")
              '';
            };
          nixos-gate-2 =
            let
              harness = import ./tests/nixos/lib.nix {
                inherit pkgs;
                telchar = self.packages.${system}.telchar;
              };
            in
            harness.mkTest {
              name = "telchar-nixos-gate-2";
              restrictedIngress = true;
              includeCollector = true;
              testScript = ''
                start_all()
                otlp_collector.wait_for_open_port(4317)
                gateway.wait_for_unit("telchar-daemon.service")
                gateway.wait_for_unit("sshd.service")
                stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
                public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
                gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
                gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
                gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
                ssh_options = "-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar-ingress@gateway"
                stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout 30 nix --extra-experimental-features nix-command --store ssh-ng://telchar-ingress@gateway store info > /tmp/nix-store-info 2>&1")
                stock_client.succeed("grep -q 'Version: telchar' /tmp/nix-store-info || { cat /tmp/nix-store-info >&2; exit 1; }")
                stock_client.succeed("timeout 10 ssh " + ssh_options + " arbitrary-command >/dev/null 2>&1 || true")
                gateway.succeed("grep -q '^original_command=arbitrary-command$' /run/telchar/forced-command-evidence")
                stock_client.succeed("TELCHAR_AUTHENTICATED_KEY=spoofed timeout 10 ssh -o SendEnv=TELCHAR_AUTHENTICATED_KEY " + ssh_options + " ignored >/dev/null 2>&1 || true")
                gateway.succeed("grep -q '^client_supplied_key=$' /run/telchar/forced-command-evidence && ! grep -q '^authenticated_key=spoofed$' /run/telchar/forced-command-evidence")
                stock_client.succeed("test $(timeout -s KILL 5 ssh -tt " + ssh_options + " true >/tmp/pty.out 2>&1; echo $?) -ne 0")
                stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -R 127.0.0.1:22346:127.0.0.1:22 -N " + ssh_options + " >/tmp/remote-forward.out 2>&1; echo $?) -ne 0")
                stock_client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -L 127.0.0.1:22345:127.0.0.1:22 -N " + ssh_options + " >/tmp/local-forward.out 2>&1; echo $?) -ne 0")
              '';
            };
          nixos-gate-3-contract =
            let
              harness = import ./tests/nixos/lib.nix {
                inherit pkgs;
                telchar = self.packages.${system}.telchar;
              };
              remoteOnlyDerivation = pkgs.writeText "telchar-remote-only-derivation.nix" ''
                let
                  source = builtins.toFile "telchar-gate-3-input" "telchar-source-input";
                in
                derivation {
                  name = "telchar-gate-3-contract";
                  system = builtins.currentSystem;
                  builder = "/bin/sh";
                  args = [ "-c" "printf telchar-gate-3-build-log >&2; test -e $source; printf telchar-source-input > $out" ];
                  inherit source;
                }
              '';
            in
            harness.mkGate3Test {
              name = "telchar-nixos-gate-3-contract";
              testScript = ''
                start_all()
                otlp_collector.wait_for_open_port(4317)
                gateway.wait_for_unit("telchar-daemon.service")
                gateway.wait_for_unit("sshd.service")
                stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
                public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
                gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
                gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
                gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
                stock_client.succeed("cp ${remoteOnlyDerivation} /tmp/remote-only.nix")
                stock_client.succeed("test $(timeout -s KILL 20 nix --extra-experimental-features nix-command build --no-link --max-jobs 0 --file /tmp/remote-only.nix > /tmp/local-build.out 2>&1; echo $?) -ne 0")
                stock_client.succeed("grep -Eqi 'unable to start any build|0 local jobs|no enabled build users|cannot build|no machines' /tmp/local-build.out || { cat /tmp/local-build.out >&2; exit 1; }")
                gateway.succeed("mkdir -p /run/telchar-direct-bin /var/lib/telchar-direct-client")
                gateway.succeed("printf '#!/bin/sh\\nset -eu\\ncase \" $* \" in *\" -O check \"*) exit 1 ;; esac\\nprintf '\"'\"'started\\n'\"'\"'\\nexec sudo -u telchar-ingress env TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock TELCHAR_AUTHENTICATED_KEY=SHA256:direct-stdio ${
                  self.packages.${system}.telchar
                }/bin/telchar serve-stdio\\n' > /run/telchar-direct-bin/ssh && chmod 755 /run/telchar-direct-bin/ssh")
                gateway.succeed("env PATH=/run/telchar-direct-bin:$PATH NIX_CONFIG='substituters =\nsandbox = false\nbuild-users-group =' timeout -s KILL 60 nix --extra-experimental-features nix-command --store 'local?root=/var/lib/telchar-direct-client' build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-direct x86_64-linux' --file ${remoteOnlyDerivation} > /tmp/direct-build.out 2>&1 || { cat /tmp/direct-build.out >&2; exit 1; }")
                gateway.succeed("grep -q 'telchar-gate-3-build-log' /tmp/direct-build.out || { cat /tmp/direct-build.out >&2; exit 1; }")
                direct_output_path = gateway.succeed("tail -n 1 /tmp/direct-build.out").strip()
                gateway.succeed("test \"$(cat /var/lib/telchar-direct-client" + direct_output_path + ")\" = telchar-source-input")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND released_at IS NULL AND store_path = '" + direct_output_path + "'\" | grep -qx 1")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose IN ('derivation', 'input') AND state = 'active'\" | grep -qx 0")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM request_attachments WHERE state = 'detached' AND detached_at IS NOT NULL\" | grep -qx 1")
                gateway.succeed("direct_output_root=$(find /var/lib/telchar-gc-roots -mindepth 1 -maxdepth 1 -type l -lname '" + direct_output_path + "' -print); test \"$(printf '%s\\n' \"$direct_output_root\" | sed '/^$/d' | wc -l)\" -eq 1")
                stock_client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 60 nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway x86_64-linux' --file /tmp/remote-only.nix > /tmp/remote-build.out 2>&1 || { cat /tmp/remote-build.out >&2; exit 1; }")
                output_path = stock_client.succeed("tail -n 1 /tmp/remote-build.out").strip()
                stock_client.succeed("test \"$(cat " + output_path + ")\" = telchar-source-input")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.query_valid_paths.completed'")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.add_multiple_to_store.completed'")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.build_derivation.admitted'")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.build_derivation.completed'")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'input' AND state = 'released' AND released_at IS NOT NULL\" | grep -qx 2")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'derivation' AND state = 'released' AND released_at IS NOT NULL\" | grep -qx 2")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose = 'output' AND state = 'active' AND released_at IS NULL\" | grep -qx 2")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM store_leases WHERE owner_kind = 'request' AND purpose IN ('derivation', 'input') AND state = 'active'\" | grep -qx 0")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT count(*) FROM request_attachments WHERE state = 'detached' AND detached_at IS NOT NULL\" | grep -qx 2")
                gateway.succeed("sudo -u postgres psql -d telchar-ingress -v ON_ERROR_STOP=1 -Atc \"SELECT store_path FROM store_leases WHERE owner_kind = 'request' AND purpose = 'input' AND state = 'released'\" > /tmp/telchar-input-leases")
                gateway.succeed("test \"$(wc -l < /tmp/telchar-input-leases)\" -eq 2")
                gateway.succeed("while IFS= read -r released_input; do test -e \"$released_input\"; grep -q telchar-source-input \"$released_input\"; done < /tmp/telchar-input-leases")
                gateway.succeed("output_roots=$(find /var/lib/telchar-gc-roots -mindepth 1 -maxdepth 1 -type l -lname '" + output_path + "' -print); test \"$(printf '%s\\n' \"$output_roots\" | sed '/^$/d' | wc -l)\" -eq 2")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.query_path_info.completed.*valid=true'")
                gateway.wait_until_succeeds("journalctl -u telchar-daemon.service --no-pager | grep -q 'worker.nar_from_path.completed'")
                gateway.succeed("grep -q '^authenticated_key=SHA256:' /run/telchar/forced-command-evidence")
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
                gateway.succeed("grep -q '^Id=telchar-artifacts-failure.service$' /var/lib/telchar-artifacts/machine-state.json")
                gateway.succeed("grep -q '^ActiveState=failed$' /var/lib/telchar-artifacts/machine-state.json")
                gateway.succeed("grep -q '^Result=exit-code$' /var/lib/telchar-artifacts/machine-state.json")
                gateway.succeed("! grep -q not-for-artifacts /var/lib/telchar-artifacts/journal.log /var/lib/telchar-artifacts/machine-state.json")
                otlp_collector.wait_until_succeeds("test -s /var/lib/telchar-otlp/records.json")
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
          nativeBuildInputs = [ pkgs.postgresql ];
          cargoTestExtraArgs = "--lib";
          postInstall = ''
            mkdir -p $out/libexec/telchar
            cp ${nixStoreBuild}/libexec/telchar/nix-store-build $out/libexec/telchar/nix-store-build
            cp ${nixStoreExport}/libexec/telchar/nix-store-export $out/libexec/telchar/nix-store-export
            cp ${nixStorePromote}/libexec/telchar/nix-store-promote $out/libexec/telchar/nix-store-promote
            cp ${nixStoreClosure}/libexec/telchar/nix-store-closure $out/libexec/telchar/nix-store-closure
          '';
        };
        nix-store-build = nixStoreBuild;
        nix-store-export = nixStoreExport;
        nix-store-promote = nixStorePromote;
        nix-store-closure = nixStoreClosure;
        nix-reference = pkgs.nix;
        default = self.packages.${system}.telchar;
      };

      devShells.${system}.default = pkgs.mkShell {
        TELCHAR_NIX = "${pkgs.nix}/bin/nix";
        TELCHAR_NIX_BIN = "${pkgs.nix}/bin/nix";
        TELCHAR_NIX_STORE_BUILD = "${nixStoreBuild}/libexec/telchar/nix-store-build";
        TELCHAR_NIX_STORE_EXPORT = "${nixStoreExport}/libexec/telchar/nix-store-export";
        TELCHAR_NIX_STORE_PROMOTE = "${nixStorePromote}/libexec/telchar/nix-store-promote";
        TELCHAR_NIX_STORE_CLOSURE = "${nixStoreClosure}/libexec/telchar/nix-store-closure";
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
