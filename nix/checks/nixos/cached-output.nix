# Exercises cached derivation results against real Nix stores and PostgreSQL.
{ pkgs, telchar, system, ... }:
let
  harness = import ../../../tests/nixos/lib.nix { inherit pkgs telchar; };
  expression = pkgs.writeText "telchar-cached-output.nix" ''
    derivation {
      name = "telchar-cached-output";
      system = "${system}";
      builder = builtins.storePath "${pkgs.runtimeShell}";
      args = [ "-c" "printf cached-output > $out" ];
    }
  '';
in
harness.mkRestrictedIngressTest {
  name = "telchar-nixos-cached-output";
  testScript = ''
    import shlex

    start_all()
    gateway.wait_for_unit("telchar-daemon.service")
    gateway.wait_for_unit("sshd.service")
    stock_client.succeed('mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N "" -f /root/.ssh/id_ed25519')
    public_key = stock_client.succeed("cat /root/.ssh/id_ed25519.pub").strip()
    gateway.succeed("mkdir -p /var/lib/telchar-ingress/.ssh")
    authorized = 'command="/etc/telchar/forced-command",restrict ' + public_key
    gateway.succeed("printf '%s\\n' " + shlex.quote(authorized) + " > /var/lib/telchar-ingress/.ssh/authorized_keys")
    gateway.succeed("chown -R telchar-ingress:telchar /var/lib/telchar-ingress/.ssh && chmod 700 /var/lib/telchar-ingress/.ssh && chmod 600 /var/lib/telchar-ingress/.ssh/authorized_keys")
    stock_client.succeed("ssh-keyscan gateway > /root/.ssh/known_hosts 2>/dev/null")
    drv = stock_client.succeed("nix-instantiate ${expression} --add-root /tmp/cached.drv").strip()
    exported = stock_client.succeed("nix-store --export " + shlex.quote(drv) + " | base64 -w0").strip()
    gateway.succeed("printf '%s' " + shlex.quote(exported) + " | base64 -d | nix-store --import >/dev/null")
    command = "HOME=/root nix-store --realise " + shlex.quote(drv) + " --max-jobs 0 --option substituters '" + "' --builders 'ssh-ng://telchar-ingress@gateway ${system}'"

    def sql(query):
        return gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc " + shlex.quote(query)).strip()

    output = stock_client.succeed(command).strip()
    assert stock_client.succeed("cat " + shlex.quote(output)) == "cached-output"
    assert sql("SELECT count(*) FROM shared_builds WHERE state = 'succeeded'") == "1"
    input_count = sql("SELECT count(*) FROM store_leases WHERE purpose IN ('derivation', 'input')")
    assert int(input_count) > 0, "cache miss retains execution inputs"
    sql("UPDATE store_leases SET expires_at = transaction_timestamp() WHERE purpose = 'output'")
    stock_client.succeed("nix-store --delete " + shlex.quote(output))
    assert stock_client.succeed(command).strip() == output
    assert sql("SELECT count(*) FROM store_leases WHERE purpose IN ('derivation', 'input')") == input_count, "cached request must not create execution input leases"
    assert sql("SELECT count(*) FROM shared_builds") == "1", "cached request does not create an execution"
    stock_client.succeed("nix-store --delete " + shlex.quote(output))
    gateway.wait_until_succeeds("sudo -u postgres psql -d telchar-ingress -Atc \"SELECT count(*) FROM store_leases WHERE purpose = 'output' AND state = 'reconciled'\" | grep -qx 1")
    gateway.succeed("nix-store --gc")
    gateway.succeed("nix-store --check-validity " + shlex.quote(output))
    stock_client.succeed("nix --extra-experimental-features nix-command copy --no-check-sigs --from ssh-ng://telchar-ingress@gateway " + shlex.quote(output))
    assert stock_client.succeed("cat " + shlex.quote(output)) == "cached-output"
  '';
}
