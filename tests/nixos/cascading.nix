# Independent gateway stores with one explicit downstream Telchar SSH backend.
{ pkgs, ingress }:
{
  mkCascadingTest =
    { name, testScript }:
    pkgs.testers.nixosTest {
      inherit name;
      nodes = {
        stock-client = ingress.restrictedIngressClientModule;
        upstream =
          { lib, ... }:
          {
            imports = [ ingress.restrictedIngressGatewayModule ];
            nix.settings.extra-sandbox-paths = [ "/etc/telchar-test-role" ];
            environment.etc.telchar-test-role.text = lib.mkForce "upstream";
            systemd.services.telchar-daemon.wantedBy = lib.mkForce [ ];
            environment.etc."telchar/telchar.toml".text = lib.mkForce ''
              [[backends.ssh]]
              system = "${pkgs.stdenv.hostPlatform.system}"
              maximum_concurrent_builds = 1
              ssh_user = "telchar-ingress"
              identity_file = "/var/lib/telchar-ingress/.ssh/downstream"
              known_hosts_file = "/var/lib/telchar-ingress/.ssh/known_hosts"

              [backends.ssh.builders]
              source = "static"

              [backends.ssh.builders.downstream]
              address = "downstream"
            '';
          };
        downstream =
          { lib, ... }:
          {
            imports = [ ingress.restrictedIngressGatewayModule ];
            nix.settings.extra-sandbox-paths = [ "/etc/telchar-test-role" ];
            environment.etc.telchar-test-role.text = lib.mkForce "downstream";
          };
      };
      testScript = ''
        start_all()
        downstream.wait_for_unit("telchar-daemon.service")
        downstream.wait_for_unit("sshd.service")
        upstream.wait_for_unit("postgresql.service")
        upstream.wait_for_unit("sshd.service")

        def authorize(source, target, key):
            source.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-ingress/.ssh")
            source.succeed("sudo -u telchar-ingress ssh-keygen -q -t ed25519 -N \"\" -f " + key)
            public_key = source.succeed("cat " + key + ".pub").strip()
            target.succeed("install -d -m 700 -o telchar-ingress -g telchar /var/lib/telchar-ingress/.ssh")
            target.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
            target.succeed("chown telchar-ingress:telchar /var/lib/telchar-ingress/.ssh/authorized_keys && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")

        authorize(upstream, downstream, "/var/lib/telchar-ingress/.ssh/downstream")
        upstream.succeed("ssh-keyscan -t ed25519 downstream > /var/lib/telchar-ingress/.ssh/known_hosts 2>/dev/null && chown telchar-ingress:telchar /var/lib/telchar-ingress/.ssh/known_hosts")
        upstream.succeed("systemctl start telchar-daemon.service")
        upstream.wait_for_unit("telchar-daemon.service")
        for gateway in (upstream, downstream):
            gateway.wait_until_succeeds("test -S /run/telchar/daemon.sock")

        stock_client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = stock_client.succeed("cat /root/.ssh/telchar.pub").strip()
        upstream.succeed("printf 'command=\"/etc/telchar/forced-command\",restrict %s\\n' '" + public_key + "' > /var/lib/telchar-ingress/.ssh/authorized_keys")
        upstream.succeed("chown telchar-ingress:telchar /var/lib/telchar-ingress/.ssh/authorized_keys && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
        stock_client.succeed("ssh-keyscan -t ed25519 upstream > /root/.ssh/known_hosts 2>/dev/null")
        ${testScript}
      '';
    };
}
