# Defines public NixOS module and local smoke checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-module = pkgs.testers.nixosTest {
    name = "telchar-nixos-module";
    nodes.gateway =
      { pkgs, ... }:
      {
        imports = [ telcharModule ];
        networking.firewall.enable = false;
        services.telchar = {
          enable = true;
          package = telchar;
          frontendUid = 995;
          settings = {
            running_disconnect_policy = "detach-and-finish";
            backends.local = {
              name = "local";
              system = system;
              maximum_concurrent_builds = 1;
            };
          };
          environment = {
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };
        environment.etc."ssh/authorized_keys.d/telchar".text =
          "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGQ5k8KfV+TWbrZG7MBXn9cKbIYB1vLLtvbCeK6ucvE3 telchar-module-test\n";
        services.telchar.openssh.authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
        system.stateVersion = "26.05";
      };
    testScript = ''
      start_all()
      gateway.wait_for_unit("postgresql.service")
      gateway.wait_for_unit("telchar.service")
      gateway.succeed("systemctl is-active sshd.service")
      gateway.succeed("systemctl is-active telchar.service || { systemctl status telchar.service --no-pager >&2; journalctl -u telchar.service --no-pager >&2; exit 1; }")
      gateway.succeed("test -S /run/telchar/daemon.sock")
      gateway.succeed("test $(stat -c %a /run/telchar) = 700")
      gateway.succeed("sudo -u postgres psql -Atc \"select 1 from pg_database where datname = 'telchar'\" | grep -qx 1")
      gateway.succeed("systemctl show telchar.service -p User --value | grep -qx telchar")
      gateway.succeed("grep -q 'ForceCommand /nix/store/' /etc/ssh/sshd_config")
    '';
  };
  nixos-test-library =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
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
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
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
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
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
}
