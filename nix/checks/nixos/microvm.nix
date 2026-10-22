# Exercises the service module with a test-only loopback provider, without AWS resources.
{ pkgs, telchar, telcharModule }:
let
  daemon = telchar.overrideAttrs (_previous: {
    pname = "telchar-microvm-service-test";
    CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS = "true";
    doCheck = false;
  });
in
pkgs.testers.nixosTest {
  name = "telchar-microvm-service";
  nodes.gateway = { ... }: {
    imports = [ telcharModule ];
    virtualisation.memorySize = 2048;
    virtualisation.cores = 1;
    systemd.tmpfiles.rules = [ "d /var/lib/telchar/gc-roots 0700 telchar telchar -" ];
    services.postgresql = {
      enable = true;
      ensureDatabases = [ "telchar" ];
      ensureUsers = [{ name = "telchar"; ensureDBOwnership = true; }];
    };
    systemd.services.microvm-provider = {
      wantedBy = [ "multi-user.target" ];
      before = [ "telchar.service" ];
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${../../../tests/nixos/microvm-provider.py}";
        DynamicUser = true;
      };
    };
    services.telchar = {
      enable = true;
      package = daemon;
      frontendUid = 995;
      database.url = "host=/run/postgresql user=telchar dbname=telchar";
      settings = {
        backends = {
          microvm_bind = "127.0.0.1:7443";
          microvm = [{
            name = "build";
            system = "aarch64-linux";
            account_id = "116392764028";
            region = "us-west-2";
            image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder";
            image_version = "1";
            maximum_concurrent_builds = 1;
            maximum_duration_seconds = 60;
            execution_timeout_seconds = 50;
            admission_timeout_seconds = 20;
            provider_timeout_seconds = 2;
            cleanup_interval_seconds = 1;
            callback_url = "wss://localhost:7444/microvm";
            callback_address = "127.0.0.1:7444";
            staging_directory = "/var/lib/telchar";
            maximum_paths = 10;
            maximum_nar_bytes = 1048576;
            maximum_total_bytes = 2097152;
            metadata_bytes = 8192;
            chunk_bytes = 262144;
          }];
        };
      };
      environment = {
        AWS_EC2_METADATA_DISABLED = "true";
        AWS_ACCESS_KEY_ID = "fixture-key";
        AWS_SECRET_ACCESS_KEY = "fixture-secret";
        TELCHAR_TEST_MICROVM_ENDPOINT = "http://127.0.0.1:9876";
      };
    };
    systemd.services.telchar = {
      after = [ "postgresql-setup.service" "microvm-provider.service" ];
      requires = [ "postgresql-setup.service" "microvm-provider.service" ];
    };
    system.stateVersion = "26.05";
  };
  testScript = ''
    start_all()
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_open_port(7443)
    gateway.succeed("test -S /run/telchar/daemon.sock")
    assert gateway.succeed("systemctl show telchar.service -p NRestarts --value").strip() == "0"
    gateway.succeed("systemctl stop telchar.service")
    gateway.wait_for_closed_port(7443)
    gateway.succeed("test ! -e /run/telchar/daemon.sock")
    gateway.succeed("systemctl start telchar.service")
    gateway.wait_for_open_port(7443)
    gateway.succeed("systemctl stop telchar.service")
    gateway.wait_for_closed_port(7443)
    service_journal = gateway.succeed("journalctl -u telchar.service -u microvm-provider.service -u postgresql.service --no-pager")
    assert not any(marker in service_journal for marker in ("Traceback", " ERROR ", "FATAL:")), service_journal
  '';
}
