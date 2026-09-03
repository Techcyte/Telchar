# Verifies atomic SSH host-certificate renewal and retention of the last valid certificate.
{
  pkgs,
  system,
  telchar,
  standaloneModule,
}:
pkgs.testers.nixosTest {
  name = "telchar-nixos-ssh-host-certificate-renewal";

  nodes = {
    postgres =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        services.postgresql = {
          enable = true;
          package = pkgs.postgresql;
          enableTCPIP = true;
          authentication = pkgs.lib.mkForce ''
            local all all trust
            host telchar telchar 0.0.0.0/0 trust
            host telchar telchar ::/0 trust
          '';
          ensureDatabases = [ "telchar" ];
          ensureUsers = [
            {
              name = "telchar";
              ensureDBOwnership = true;
            }
          ];
        };
        system.stateVersion = "26.05";
      };

    client =
      { pkgs, ... }:
      {
        environment.systemPackages = [ pkgs.openssh ];
        system.stateVersion = "26.05";
      };

    gateway =
      { pkgs, ... }:
      {
        imports = [ standaloneModule ];

        networking.extraHosts = "192.168.1.1 postgres";
        services.openssh.enable = true;
        services.telchar = {
          package = telchar;
          database.url = "postgresql://telchar@postgres/telchar";
          ingress.openssh = {
            hostKeyFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key";
            authorizedKeysFile = "/etc/ssh/authorized_keys.d/telchar";
            hostCertificateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub";
            trustedUserCAKeysFile = "/var/lib/telchar/ssh/client-ca.pub";
            authorizedPrincipalsFile = "/var/lib/telchar/ssh/authorized_principals";
          };
          sshHostCertificateRenewal = {
            enable = true;
            candidateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub";
          };
          settings.backends.local = {
            name = "local";
            inherit system;
            maximum_concurrent_builds = 1;
          };
          environment = {
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_NIX = "${pkgs.nix}/bin/nix";
          };
        };

        environment.etc."ssh/authorized_keys.d/telchar".text = "";
        systemd.services.telchar-sshd.wantedBy = pkgs.lib.mkForce [ ];
        systemd.services.telchar-sshd.restartIfChanged = false;
        system.stateVersion = "26.05";
      };
  };

  testScript = ''
    postgres.start()
    postgres.wait_for_unit("postgresql.service")
    client.start()
    client.succeed("install -d -m 700 /root/.ssh")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/trusted-ca")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/untrusted-ca")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/trusted-client")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/untrusted-client")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/unsigned-client")
    client.succeed("ssh-keygen -q -s /root/.ssh/trusted-ca -I trusted-client -n telchar-builder -V -1m:+10m /root/.ssh/trusted-client.pub")
    client.succeed("ssh-keygen -q -s /root/.ssh/untrusted-ca -I untrusted-client -n telchar-builder -V -1m:+10m /root/.ssh/untrusted-client.pub")
    trusted_client_ca = client.succeed("cat /root/.ssh/trusted-ca.pub").strip()

    gateway.start()
    gateway.succeed("systemctl stop telchar.service telchar-sshd.service || true")
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/ssh")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/ssh_host_ed25519_key")
    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/host-ca")
    gateway.succeed("printf '%s\\n' '" + trusted_client_ca + "' > /var/lib/telchar/ssh/client-ca.pub")
    gateway.succeed("printf 'telchar-builder\\n' > /var/lib/telchar/ssh/authorized_principals")
    gateway.succeed("ssh-keygen -q -s /var/lib/telchar/ssh/host-ca -I initial -h -n gateway -V -1m:+5m /var/lib/telchar/ssh/ssh_host_ed25519_key.pub")
    gateway.succeed("chown -R telchar:telchar /var/lib/telchar/ssh && chmod 600 /var/lib/telchar/ssh/ssh_host_ed25519_key /var/lib/telchar/ssh/host-ca && chmod 644 /var/lib/telchar/ssh/*.pub /var/lib/telchar/ssh/authorized_principals")
    gateway.succeed("systemctl daemon-reload")
    gateway.succeed("systemctl reset-failed telchar.service")
    gateway.succeed("systemctl start telchar.service telchar-sshd.service")
    gateway.wait_for_unit("telchar-sshd.service")
    client.succeed("timeout 10 ssh -vv -p 2222 -i /root/.ssh/trusted-client -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar@gateway </dev/null >/tmp/trusted.out 2>&1 || true; grep -q 'Authenticated to gateway' /tmp/trusted.out")
    client.fail("timeout 10 ssh -vv -p 2222 -i /root/.ssh/untrusted-client -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar@gateway </dev/null >/tmp/untrusted.out 2>&1; grep -q 'Authenticated to gateway' /tmp/untrusted.out")
    client.fail("timeout 10 ssh -vv -p 2222 -i /root/.ssh/unsigned-client -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null telchar@gateway </dev/null >/tmp/unsigned.out 2>&1; grep -q 'Authenticated to gateway' /tmp/unsigned.out")

    original_key = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key").split()[0]
    original_certificate = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub").split()[0]
    original_sshd_pid = gateway.succeed("systemctl show telchar-sshd.service -p MainPID --value").strip()

    gateway.succeed("ssh-keygen -q -s /var/lib/telchar/ssh/host-ca -I renewed -h -n gateway -V -1m:+10m /var/lib/telchar/ssh/ssh_host_ed25519_key.pub && mv /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub && chown telchar:telchar /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub")
    gateway.succeed("systemctl start telchar-ssh-host-certificate-renewal.service")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key | cut -d' ' -f1) = " + original_key)
    gateway.fail("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | cut -d' ' -f1) = " + original_certificate)
    gateway.succeed("ssh-keygen -L -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | grep -q 'renewed'")
    gateway.succeed("test $(systemctl show telchar-sshd.service -p MainPID --value) = " + original_sshd_pid)
    gateway.succeed("systemctl is-active --quiet telchar-sshd.service")

    renewed_certificate = gateway.succeed("sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub").split()[0]
    gateway.succeed("install -d -m 700 /root/private-candidate && cp /var/lib/telchar/ssh/ssh_host_ed25519_key.pub /root/private-candidate/host-key.pub && ssh-keygen -q -s /var/lib/telchar/ssh/host-ca -I root-private -h -n gateway -V -1m:+10m /root/private-candidate/host-key.pub && rm -f /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub && ln -s /root/private-candidate/host-key-cert.pub /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub")
    gateway.fail("systemctl start telchar-ssh-host-certificate-renewal.service")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | cut -d' ' -f1) = " + renewed_certificate)
    gateway.succeed("test $(systemctl show telchar-sshd.service -p MainPID --value) = " + original_sshd_pid)
    gateway.succeed("systemctl is-active --quiet telchar-sshd.service")
    gateway.succeed("rm /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub")

    gateway.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /var/lib/telchar/ssh/unrelated_host_key")
    gateway.succeed("ssh-keygen -q -s /var/lib/telchar/ssh/host-ca -I invalid -h -n gateway -V -1m:+10m /var/lib/telchar/ssh/unrelated_host_key.pub && mv /var/lib/telchar/ssh/unrelated_host_key-cert.pub /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub")
    gateway.fail("systemctl start telchar-ssh-host-certificate-renewal.service")
    gateway.succeed("systemctl is-failed --quiet telchar-ssh-host-certificate-renewal.service")
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub | cut -d' ' -f1) = " + renewed_certificate)
    gateway.succeed("test $(sha256sum /var/lib/telchar/ssh/ssh_host_ed25519_key | cut -d' ' -f1) = " + original_key)
    gateway.succeed("test $(systemctl show telchar-sshd.service -p MainPID --value) = " + original_sshd_pid)
    gateway.succeed("systemctl is-active --quiet telchar-sshd.service")
  '';
}
