# Competing daemon hosts share PostgreSQL but retain separate local stores for takeover tests.
{
  pkgs,
  telchar,
  common,
}:
let
  inherit (common) machineModule;
  restartDatabaseModule = machineModule {
    role = "postgres";
    extraConfig = {
      services.postgresql = {
        enable = true;
        package = pkgs.postgresql;
        enableTCPIP = true;
        ensureDatabases = [ "telchar-recovery" ];
        ensureUsers = [
          {
            name = "telchar-recovery";
            ensureDBOwnership = true;
          }
        ];
        authentication = pkgs.lib.mkForce ''
          local all all trust
          host all all 0.0.0.0/0 trust
          host all all ::/0 trust
        '';
      };
      networking.firewall.enable = false;
    };
  };

  restartDaemonModule =
    { role }:
    machineModule {
      inherit role;
      extraConfig = {
        environment.systemPackages = [ telchar ];
        environment.etc."telchar/telchar.toml".text = ''
          [database]
          ownership_renewal_seconds = 1
          ownership_lease_seconds = 3

          [backends.local]
          name = "local"
          system = "${pkgs.stdenv.hostPlatform.system}"
          maximum_concurrent_builds = 1
        '';
        systemd.services.telchar-recovery-daemon = {
          description = "Telchar restart recovery daemon";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          environment = {
            TELCHAR_DATABASE_URL = "postgresql://telchar-recovery@postgres/telchar-recovery";
            TELCHAR_GATEWAY_DISK_RESERVE_BYTES = "1048576";
            TELCHAR_GATEWAY_STORE_URI = "unix:///nix/var/nix/daemon-socket/socket";
            TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = "/var/lib/telchar-recovery-roots";
            TELCHAR_CONFIG = "/etc/telchar/telchar.toml";
            NIX_CONFIG = ''
              post-build-hook =
              substituters =
            '';
          };
          serviceConfig = {
            Type = "simple";
            StateDirectory = "telchar-recovery-roots";
            StateDirectoryMode = "0700";
            ExecStart = "${telchar}/bin/telchar daemon --socket /run/telchar-recovery/daemon.sock --frontend-uid 0";
            RuntimeDirectory = "telchar-recovery";
            RuntimeDirectoryMode = "0700";
            Restart = "no";
          };
        };
      };
    };

in
{
  mkRestartRecoveryTest =
    {
      name,
      testScript ? "",
    }:
    pkgs.testers.nixosTest {
      inherit name testScript;
      nodes = {
        postgres = restartDatabaseModule;
        owner = restartDaemonModule { role = "owner"; };
        replacement = restartDaemonModule { role = "replacement"; };
      };
    };

}
