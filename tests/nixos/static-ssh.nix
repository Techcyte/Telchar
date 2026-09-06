# Pinned SSH builder identities and feature-separated targets for remote execution contracts.
{
  pkgs,
  common,
  ingress,
}:
let
  inherit (common) machineModule;
  inherit (ingress) restrictedIngressGatewayModule restrictedIngressClientModule;
  staticSshBuilderModule =
    {
      role ? "static-ssh-builder",
      account ? "telchar-builder",
      uid ? 994,
      evidence ? "/var/lib/telchar-builder/forced-command-evidence",
      systemFeatures ? [ ],
    }:
    machineModule {
      inherit role;
      extraConfig = {
        environment.systemPackages = [ pkgs.nix ];
        services.openssh = {
          enable = true;
          settings = {
            PasswordAuthentication = false;
            KbdInteractiveAuthentication = false;
            PermitRootLogin = "no";
            PermitTTY = false;
            AllowTcpForwarding = false;
            AllowAgentForwarding = false;
            X11Forwarding = false;
            PermitUserEnvironment = false;
          };
        };
        users.users.${account} = {
          isSystemUser = true;
          inherit uid;
          group = account;
          home = "/var/lib/${account}";
          createHome = true;
          shell = "${pkgs.bashInteractive}/bin/bash";
        };
        users.groups.${account} = { };
        nix.settings = {
          trusted-users = [
            "root"
            account
          ];
          system-features = systemFeatures;
        };
        environment.etc."telchar-static-ssh/forced-command" = {
          mode = "0555";
          text = ''
            #!${pkgs.runtimeShell}
            set -eu
            evidence=${evidence}
            printf 'original_command=%s agent_socket=%s display=%s\n' \
              "''${SSH_ORIGINAL_COMMAND-}" "''${SSH_AUTH_SOCK-}" "''${DISPLAY-}" >> "$evidence"
            case "''${SSH_ORIGINAL_COMMAND-}" in
              "nix-daemon --stdio") exec ${pkgs.nix}/bin/nix-daemon --stdio ;;
              "*/nix-daemon --stdio") exec ${pkgs.nix}/bin/nix-daemon --stdio ;;
              *) exit 126 ;;
            esac
          '';
        };
        systemd.tmpfiles.rules = [
          "f ${evidence} 0600 ${account} ${account} -"
        ];
        environment.etc."ssh/sshd_config.d/telchar-static-builder.conf".text = ''
          Match User ${account}
            AuthorizedKeysFile /var/lib/${account}/.ssh/authorized_keys
            ForceCommand /etc/telchar-static-ssh/forced-command
            DisableForwarding yes
            PermitTTY no
            PermitUserEnvironment no
        '';
      };
    };

  staticSshClientModule = machineModule {
    role = "static-ssh-client";
    extraConfig = {
      environment.systemPackages = [
        pkgs.nix
        pkgs.openssh
      ];
    };
  };

  staticSshGatewayModule =
    { ... }:
    {
      imports = [ restrictedIngressGatewayModule ];
      systemd.services.telchar-daemon.wantedBy = pkgs.lib.mkForce [ ];
      systemd.services.telchar-daemon.environment.TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
      systemd.tmpfiles.rules = [
        "d /var/lib/telchar-static-ssh 0700 telchar-ingress telchar -"
      ];
      environment.etc."telchar/telchar.toml".text = ''
        [[backends.ssh]]
        system = "${pkgs.stdenv.hostPlatform.system}"
        maximum_concurrent_builds = 1
        ssh_user = "telchar-builder"
        identity_file = "/var/lib/telchar-static-ssh/identity"
        known_hosts_file = "/var/lib/telchar-static-ssh/known-hosts"

        [backends.ssh.builders]
        source = "static"

        [backends.ssh.builders.builder]
        address = "builder"
      '';
    };

in
{
  # Proves stock Nix can reach a pinned builder while shell commands, PTYs,
  # TCP forwarding, and forwarded agent/display access remain unavailable.
  # This is an SSH fixture contract; no Telchar daemon participates.
  mkStaticSshFixtureTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        client = staticSshClientModule;
        builder = staticSshBuilderModule { };
      };
      testScript = ''
        start_all()
        builder.wait_for_unit("sshd.service")
        client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar-builder")
        public_key = client.succeed("cat /root/.ssh/telchar-builder.pub").strip()
        builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
        builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
        builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        client.succeed("ssh-keyscan -t ed25519 builder > /root/.ssh/known_hosts 2>/dev/null")
        ssh_options = "-i /root/.ssh/telchar-builder -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts telchar-builder@builder"
        client.succeed("HOME=/root NIX_SSHOPTS='-i /root/.ssh/telchar-builder -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts' timeout 30 nix --extra-experimental-features nix-command store ping --store ssh-ng://telchar-builder@builder > /tmp/store-ping 2>&1")
        client.succeed("grep -q 'Store URL: ssh-ng://telchar-builder@builder' /tmp/store-ping || { cat /tmp/store-ping >&2; exit 1; }")
        builder.succeed("grep -Eq '^original_command=.*/?nix-daemon --stdio agent_socket= display=$' /var/lib/telchar-builder/forced-command-evidence || { cat /var/lib/telchar-builder/forced-command-evidence >&2; exit 1; }")
        client.fail("timeout 10 ssh " + ssh_options + " true")
        builder.succeed("grep -q '^original_command=true agent_socket= display=$' /var/lib/telchar-builder/forced-command-evidence")
        client.succeed("test $(timeout -s KILL 5 ssh -tt " + ssh_options + " true >/tmp/pty.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -L 127.0.0.1:22345:127.0.0.1:22 -N " + ssh_options + " >/tmp/local-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(timeout -s KILL 5 ssh -o ExitOnForwardFailure=yes -R 127.0.0.1:22346:127.0.0.1:22 -N " + ssh_options + " >/tmp/remote-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("eval $(ssh-agent -s) >/tmp/agent-env && ssh-add /root/.ssh/telchar-builder >/dev/null")
        client.succeed("test $(timeout -s KILL 5 ssh -A " + ssh_options + " true >/tmp/agent-forward.out 2>&1; echo $?) -ne 0")
        client.succeed("test $(DISPLAY=:99 timeout -s KILL 5 ssh -X " + ssh_options + " true >/tmp/x11.out 2>&1; echo $?) -ne 0")
        builder.succeed("! grep -Eq 'agent_socket=[^ ]+|display=[^ ]+' /var/lib/telchar-builder/forced-command-evidence")
        ${testScript}
      '';
    };

  # Supplies separate client, gateway, and builder stores for the caller's
  # remote-build, live-log, and gateway-output validation assertions.
  mkStaticSshBuildTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        stock-client = restrictedIngressClientModule;
        gateway = staticSshGatewayModule;
        builder = staticSshBuilderModule { };
      };
      testScript = ''
        start_all()
        builder.wait_for_unit("sshd.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-static-ssh")
        gateway.succeed("sudo -u telchar-ingress ${pkgs.openssh}/bin/ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar-static-ssh/identity")
        public_key = gateway.succeed("cat /var/lib/telchar-static-ssh/identity.pub").strip()
        builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
        builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
        builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        gateway.succeed("${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder > /var/lib/telchar-static-ssh/known-hosts 2>/dev/null && chown telchar-ingress:telchar /var/lib/telchar-static-ssh/known-hosts && chmod 644 /var/lib/telchar-static-ssh/known-hosts")
        gateway.succeed("systemctl start telchar-daemon.service")
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        ingress_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + ingress_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ${testScript}
      '';
    };

  # Two builders advertise disjoint features so the caller can distinguish
  # shared-request coalescing from routing distinct builds to compatible targets.
  mkStaticSshGatewayTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        stock-client = restrictedIngressClientModule;
        gateway =
          { ... }:
          {
            imports = [ restrictedIngressGatewayModule ];
            systemd.services.telchar-daemon.wantedBy = pkgs.lib.mkForce [ ];
            systemd.services.telchar-daemon.environment.TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
            systemd.tmpfiles.rules = [
              "d /var/lib/telchar-static-ssh 0700 telchar-ingress telchar -"
            ];
            environment.etc."telchar/telchar.toml".text = ''
              [[backends.ssh]]
              system = "${pkgs.stdenv.hostPlatform.system}"
              maximum_concurrent_builds = 1
              ssh_user = "telchar-builder"
              identity_file = "/var/lib/telchar-static-ssh/identity"
              known_hosts_file = "/var/lib/telchar-static-ssh/known-hosts"

              [backends.ssh.builders]
              source = "static"

              [backends.ssh.builders.primary]
              address = "builder-primary"
              supported_features = ["primary"]

              [backends.ssh.builders.secondary]
              address = "builder-secondary"
              supported_features = ["secondary"]
            '';
          };
        builder-primary = staticSshBuilderModule {
          role = "static-ssh-builder-primary";
          systemFeatures = [ "primary" ];
        };
        builder-secondary = staticSshBuilderModule {
          role = "static-ssh-builder-secondary";
          systemFeatures = [ "secondary" ];
        };
      };
      testScript = ''
        start_all()
        builder_primary.wait_for_unit("sshd.service")
        builder_secondary.wait_for_unit("sshd.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-static-ssh")
        gateway.succeed("sudo -u telchar-ingress ${pkgs.openssh}/bin/ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar-static-ssh/identity")
        public_key = gateway.succeed("cat /var/lib/telchar-static-ssh/identity.pub").strip()
        for builder in [builder_primary, builder_secondary]:
            builder.succeed("mkdir -p /var/lib/telchar-builder/.ssh")
            builder.succeed("printf 'command=\"/etc/telchar-static-ssh/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-builder/.ssh/authorized_keys")
            builder.succeed("chown -R telchar-builder:telchar-builder /var/lib/telchar-builder/.ssh && chmod 700 /var/lib/telchar-builder/.ssh && chmod 600 /var/lib/telchar-builder/.ssh/authorized_keys")
        gateway.succeed("(${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder-primary; ${pkgs.openssh}/bin/ssh-keyscan -t ed25519 builder-secondary) > /var/lib/telchar-static-ssh/known-hosts 2>/dev/null && chown telchar-ingress:telchar /var/lib/telchar-static-ssh/known-hosts && chmod 644 /var/lib/telchar-static-ssh/known-hosts")
        gateway.succeed("systemctl start telchar-daemon.service")
        gateway.wait_for_unit("telchar-daemon.service")
        gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")
        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        ingress_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
        gateway.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + ingress_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        ${testScript}
      '';
    };

}
