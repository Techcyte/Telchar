# Defines journal, machine-state, and collector artifact checks.
{
  pkgs,
  system,
  telchar,
  nomadWorker,
  telcharModule,
}:
{
  nixos-artifacts =
    let
      harness = import ../../../tests/nixos/lib.nix {
        inherit pkgs;
        telchar = telchar;
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
}
