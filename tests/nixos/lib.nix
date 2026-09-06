# Provides reusable NixOS VM node topologies and test constructors for Telchar integration checks.
{ pkgs, telchar }:
let
  common = import ./common.nix { inherit pkgs telchar; };
  ingress = import ./ingress.nix { inherit pkgs telchar common; };
  staticSsh = import ./static-ssh.nix { inherit pkgs common ingress; };
  nomad = import ./nomad.nix { inherit pkgs ingress; };
  recovery = import ./recovery.nix { inherit pkgs telchar common; };
in
{
  modules = {
    gateway = common.gatewayModule;
    stock-client = common.stockClientModule;
    otlp-collector = common.collectorModule;
  };

  helpers = {
    waitForTelchar = machine: "${machine}.wait_for_unit(\"telchar.service\")";
    assertNetwork = source: destination: "${source}.succeed(\"ping -c 1 ${destination}\")";
  };

  inherit (ingress) mkTest mkRestrictedIngressTest mkLixRestrictedIngressTest;
  inherit (staticSsh) mkStaticSshFixtureTest mkStaticSshBuildTest mkStaticSshGatewayTest;
  inherit (nomad) mkNomadFixtureTest mkNomadGatewayTest;
  inherit (recovery) mkRestartRecoveryTest;
}
