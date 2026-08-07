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
      nixStoreExport = mkNixStoreHelper "export" ./tools/nix-store-export;
      nixStorePromote = mkNixStoreHelper "promote" ./tools/nix-store-promote;
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
                derivation {
                  name = "telchar-gate-3-contract";
                  system = builtins.currentSystem;
                  builder = "/bin/sh";
                  args = [ "-c" "printf telchar-remote-build > $out" ];
                }
              '';
            in
            harness.mkTest {
              name = "telchar-nixos-gate-3-contract";
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
                stock_client.succeed("cp ${remoteOnlyDerivation} /tmp/remote-only.nix")
                stock_client.succeed("test $(timeout -s KILL 20 nix --extra-experimental-features nix-command build --no-link --max-jobs 0 --file /tmp/remote-only.nix > /tmp/local-build.out 2>&1; echo $?) -ne 0")
                stock_client.succeed("grep -Eqi 'unable to start any build|0 local jobs|no enabled build users|cannot build|no machines' /tmp/local-build.out || { cat /tmp/local-build.out >&2; exit 1; }")
                stock_client.succeed("test $(HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' timeout -s KILL 30 nix --extra-experimental-features nix-command build --no-link --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway x86_64-linux' --file /tmp/remote-only.nix > /tmp/remote-build.out 2>&1; echo $?) -ne 0")
                stock_client.succeed("grep -q 'unsupported worker operation' /tmp/remote-build.out || { cat /tmp/remote-build.out >&2; exit 1; }")
                gateway.succeed("journalctl -u telchar-daemon.service --no-pager | grep -q 'event=worker.query_valid_paths.completed' || { journalctl -u telchar-daemon.service --no-pager >&2; exit 1; }")
                gateway.succeed("journalctl -u telchar-daemon.service --no-pager | grep -q 'operation=IsValidPath' || { journalctl -u telchar-daemon.service --no-pager >&2; exit 1; }")
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
        nix-store-export = nixStoreExport;
        nix-store-promote = nixStorePromote;
        nix-reference = pkgs.nix;
        default = self.packages.${system}.telchar;
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
