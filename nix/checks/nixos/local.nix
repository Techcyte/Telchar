# Defines stock-Nix and Lix local backend compatibility checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-lix-local =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      classic = pkgs.writeText "telchar-lix-classic.nix" ''
        derivation {
          name = "telchar-lix-classic";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-lix-classic > $out" ];
        }
      '';
      flat = pkgs.writeText "telchar-lix-fixed-flat.nix" ''
        derivation {
          name = "telchar-lix-fixed-flat";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-lix-fixed-flat > $out" ];
          outputHashMode = "flat";
          outputHashAlgo = "sha256";
          outputHash = "1e830648babc5018aa0b2ecdeec06bb0ec34c3aa54420b9223436583cebae0ff";
        }
      '';
      recursive = pkgs.writeText "telchar-lix-fixed-recursive.nix" ''
        derivation {
          name = "telchar-lix-fixed-recursive";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-lix-fixed-recursive > $out" ];
          outputHashMode = "recursive";
          outputHashAlgo = "sha256";
          outputHash = "02fbfedfd215d36c4ceb1fbd22163d543bbc596bd0cda71f15c922858cd79117";
        }
      '';
      incorrect = pkgs.writeText "telchar-lix-fixed-incorrect.nix" ''
        derivation {
          name = "telchar-lix-fixed-incorrect";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf incorrect > $out" ];
          outputHashMode = "flat";
          outputHashAlgo = "sha256";
          outputHash = "0000000000000000000000000000000000000000000000000000000000000000";
        }
      '';
    in
    harness.mkLixGate3Test {
      name = "telchar-nixos-lix-local";
      testScript = ''
        start_all()
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_for_unit("sshd.service")
        stock_client.succeed("nix --version | grep -qi lix")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/id_ed25519")
        public_key = stock_client.succeed("cat /root/.ssh/id_ed25519.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        stock_client.succeed("ssh-keyscan gateway > /root/.ssh/known_hosts 2>/dev/null")
        stock_client.succeed("HOME=/root timeout 30 nix-store --store ssh-ng://telchar-ingress@gateway --version >/tmp/lix-store-version")
        for expression, expected in [("${classic}", "telchar-lix-classic"), ("${flat}", "telchar-lix-fixed-flat"), ("${recursive}", "telchar-lix-fixed-recursive")]:
            stock_client.succeed("cp " + expression + " /tmp/lix-build.nix")
            derivation_path = stock_client.succeed("nix-instantiate /tmp/lix-build.nix").strip()
            derivation_export = stock_client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
            gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
            command = "HOME=/root NIX_CONFIG='substituters =' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${system}' '" + derivation_path + "^*'"
            output_path = stock_client.succeed(command).strip()
            gateway.succeed("test \"$(cat '" + output_path + "')\" = " + expected)
        stock_client.succeed("cp ${incorrect} /tmp/lix-fixed-incorrect.nix")
        incorrect_derivation = stock_client.succeed("nix-instantiate /tmp/lix-fixed-incorrect.nix").strip()
        incorrect_export = stock_client.succeed("nix-store --export '" + incorrect_derivation + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + incorrect_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        incorrect_command = "HOME=/root NIX_CONFIG='substituters =' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${system}' '" + incorrect_derivation + "^*'"
        stock_client.fail(incorrect_command)
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select state from shared_builds where derivation_path = '" + incorrect_derivation + "'\" | grep -qx failed")
      '';
    };
  nixos-fixed-output-local =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
      };
      flat = pkgs.writeText "telchar-fixed-output-flat.nix" ''
        derivation {
          name = "telchar-fixed-flat";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-fixed-flat > $out" ];
          outputHashMode = "flat";
          outputHashAlgo = "sha256";
          outputHash = "1400917ed21ab5261be26c3dfe995fb264feed054a5981770ff199eae147b654";
        }
      '';
      recursive = pkgs.writeText "telchar-fixed-output-recursive.nix" ''
        derivation {
          name = "telchar-fixed-recursive";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-fixed-recursive > $out" ];
          outputHashMode = "recursive";
          outputHashAlgo = "sha256";
          outputHash = "fb0394a19d9c14fcf296ae79ea0a8ede66eafe00fd8904dac9046f1245f7a435";
        }
      '';
      incorrect = pkgs.writeText "telchar-fixed-output-incorrect.nix" ''
        derivation {
          name = "telchar-fixed-incorrect";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf incorrect > $out" ];
          outputHashMode = "flat";
          outputHashAlgo = "sha256";
          outputHash = "0000000000000000000000000000000000000000000000000000000000000000";
        }
      '';
    in
    harness.mkGate3Test {
      name = "telchar-nixos-fixed-output-local";
      testScript = ''
        start_all()
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_for_unit("sshd.service")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\\\"/etc/telchar/forced-command\\\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        for expression, expected in [("${flat}", "telchar-fixed-flat"), ("${recursive}", "telchar-fixed-recursive")]:
            stock_client.succeed("cp " + expression + " /tmp/fixed-output.nix")
            derivation_path = stock_client.succeed("nix-instantiate /tmp/fixed-output.nix").strip()
            derivation_export = stock_client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
            gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
            command = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${system}' '" + derivation_path + "^*'"
            output_path = stock_client.succeed(command).strip()
            gateway.succeed("test \"$(cat '" + output_path + "')\" = " + expected)
        stock_client.succeed("cp ${incorrect} /tmp/fixed-output-incorrect.nix")
        incorrect_derivation = stock_client.succeed("nix-instantiate /tmp/fixed-output-incorrect.nix").strip()
        incorrect_export = stock_client.succeed("nix-store --export '" + incorrect_derivation + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + incorrect_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        incorrect_command = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@gateway ${system}' '" + incorrect_derivation + "^*'"
        stock_client.fail(incorrect_command)
        gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select state from shared_builds where derivation_path = '" + incorrect_derivation + "'\" | grep -qx failed")
      '';
    };
}
