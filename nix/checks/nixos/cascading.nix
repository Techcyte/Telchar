# Qualifies explicit Telchar-to-Telchar execution without shared mutable stores.
{ pkgs, telchar, ... }:
let
  harness = import ../../../tests/nixos/lib.nix { inherit pkgs telchar; };
  workloads = pkgs.writeText "telchar-cascading.nix" ''
    let
      make = name: script: derivation {
        inherit name;
        system = builtins.currentSystem;
        builder = builtins.storePath "${pkgs.runtimeShell}";
        args = [ "-c" script ];
      };
    in {
      success = make "telchar-cascading-success" "printf cascading-build-log\\n >&2; ${pkgs.coreutils}/bin/cat /etc/telchar-test-role > $out";
      failure = make "telchar-cascading-failure" "printf cascading-failure-log\\n >&2; exit 1";
    }
  '';
in
harness.mkCascadingTest {
  name = "telchar-nixos-cascading";
  testScript = ''
    build = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts' timeout -s KILL 90 nix --extra-experimental-features nix-command build --no-link --print-build-logs --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar-ingress@upstream ${pkgs.stdenv.hostPlatform.system}'"
    for attribute, state in (("success", "succeeded"), ("failure", "failed")):
        drv = stock_client.succeed("nix-instantiate ${workloads} -A " + attribute).strip()
        output = stock_client.succeed("nix-store -q --outputs '" + drv + "'").strip()
        for machine in (stock_client, upstream, downstream):
            machine.succeed("test ! -e '" + output + "'")
        # Both gateways know the derivation before execution, so locality does
        # not depend on Nix declining dispatch for an unregistered derivation.
        exported = stock_client.succeed("nix-store --export '" + drv + "' | base64 -w0").strip()
        for gateway in (upstream, downstream):
            gateway.succeed("printf '%s' '" + exported + "' | base64 -d | nix-store --import >/dev/null")
        command = build + " '" + drv + "^*' > /tmp/cascading-" + attribute + ".log 2>&1"
        if attribute == "success":
            stock_client.succeed(command + " || { cat /tmp/cascading-success.log >&2; exit 1; }")
            for machine in (stock_client, upstream, downstream):
                machine.succeed("nix-store --verify-path '" + output + "'")
                machine.succeed("test $(cat '" + output + "') = downstream")
            stock_client.succeed("grep -Fq cascading-build-log /tmp/cascading-success.log")
        else:
            status, diagnostic = stock_client.execute(command)
            assert status not in (0, 124, 137), (status, diagnostic)
            stock_client.succeed("grep -Fq cascading-failure-log /tmp/cascading-failure.log")
            for machine in (stock_client, upstream, downstream):
                machine.succeed("test ! -e '" + output + "'")
        for gateway in (upstream, downstream):
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_builds where derivation_path = '" + drv + "' and state = '" + state + "'\" | grep -qx 1")
            gateway.succeed("sudo -u postgres psql -d telchar-ingress -Atc \"select count(*) from shared_build_attempts where derivation_path = '" + drv + "'\" | grep -qx 1")
  '';
}
