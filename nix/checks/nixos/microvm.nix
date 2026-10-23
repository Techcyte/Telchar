# Exercises the service module with a test-only loopback provider, without AWS resources.
{ pkgs, telchar, nomadWorker, telcharModule }:
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
    virtualisation.diskSize = 16384;
    virtualisation.writableStoreUseTmpfs = false;
    networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkForce [{ address = "192.168.1.2"; prefixLength = 24; }];
    networking.firewall.enable = false;
    nix.settings.trusted-users = [ "root" "telchar" ];
    nix.settings.substituters = pkgs.lib.mkForce [];
    environment.systemPackages = [ pkgs.openssl ];
    users.users.telchar.shell = pkgs.bashInteractive;
    services.openssh.enable = true;
    services.openssh.settings.PasswordAuthentication = false;
    services.nginx = {
      enable = true;
      virtualHosts.gateway = {
        onlySSL = true;
        listen = [{ addr = "192.168.1.2"; port = 7444; ssl = true; }];
        sslCertificate = "/run/microvm-tls/cert.pem";
        sslCertificateKey = "/run/microvm-tls/key.pem";
        locations."/microvm" = {
          proxyPass = "http://127.0.0.1:7443";
          proxyWebsockets = true;
        };
      };
    };
    systemd.services.nginx.wantedBy = pkgs.lib.mkForce [];
    environment.etc."microvm-serve".source = pkgs.writeShellScript "microvm-serve" ''
      export TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock
      export TELCHAR_AUTHENTICATED_KEY=SHA256:microvm-client
      exec ${daemon}/bin/telchar serve-stdio
    '';
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
            cleanup_interval_seconds = 60;
            callback_url = "wss://gateway:7444/microvm";
            callback_address = "192.168.1.2:7444";
            staging_directory = "/var/lib/telchar";
            maximum_paths = 64;
            maximum_nar_bytes = 67108864;
            maximum_total_bytes = 134217728;
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
  nodes.worker = { ... }: {
    virtualisation.memorySize = 1024;
    virtualisation.cores = 1;
    networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkForce [{ address = "192.168.1.3"; prefixLength = 24; }];
    networking.firewall.enable = false;
    nix.settings = {
      extra-platforms = [ "aarch64-linux" ];
      sandbox = true;
      sandbox-fallback = false;
      substituters = pkgs.lib.mkForce [];
      max-jobs = 1;
      cores = 1;
    };
    systemd.services.microvm-worker = {
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${../../../tests/nixos/microvm-worker.py} ${nomadWorker}/bin/telchar-microvm-worker";
        RuntimeDirectory = "microvm-worker";
        RuntimeDirectoryPreserve = true;
        UMask = "0077";
      };
    };
    system.stateVersion = "26.05";
  };
  nodes.client = { ... }: {
    virtualisation.memorySize = 512;
    virtualisation.cores = 1;
    networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkForce [{ address = "192.168.1.4"; prefixLength = 24; }];
    nix.settings = { substituters = pkgs.lib.mkForce []; experimental-features = [ "nix-command" ]; };
    environment.etc."microvm-build.nix".text = ''
      derivation {
        name = "microvm-service-output";
        system = "aarch64-linux";
        builder = "${pkgs.bash}/bin/bash";
        builderPackage = builtins.storePath "${pkgs.bash}";
        args = [ "-c" "test $EUID != 0 && printf 'microvm-service-success\\n' > $out" ];
      }
    '';
    system.stateVersion = "26.05";
  };
  testScript = ''
    import os
    start_all()
    gateway.wait_for_unit("telchar.service")
    gateway.wait_for_open_port(7443)
    gateway.succeed("test -S /run/telchar/daemon.sock")
    assert gateway.succeed("systemctl show telchar.service -p NRestarts --value").strip() == "0"
    gateway.succeed("mkdir -p /run/microvm-tls; openssl req -x509 -newkey rsa:2048 -nodes -keyout /run/microvm-tls/key.pem -out /run/microvm-tls/cert.pem -days 1 -subj /CN=gateway -addext subjectAltName=DNS:gateway -addext basicConstraints=critical,CA:FALSE 2>/run/microvm-tls/generation.log")
    gateway.succeed("chown nginx:nginx /run/microvm-tls/key.pem; chmod 600 /run/microvm-tls/key.pem")
    gateway.succeed("openssl x509 -in /run/microvm-tls/cert.pem -outform DER -out /run/microvm-tls/trust.der")
    gateway.copy_from_machine("/run/microvm-tls/trust.der")
    worker.succeed("mkdir -p /run/microvm-worker")
    worker.copy_from_host(os.environ["out"] + "/trust.der", "/run/microvm-worker/trust.der")
    worker.succeed("systemctl start microvm-worker.service")
    worker.wait_for_open_port(9877, addr="192.168.1.3", timeout=30)
    gateway.succeed("systemctl start nginx.service")
    gateway.wait_for_open_port(7444, addr="192.168.1.2", timeout=30)
    worker.succeed("${pkgs.python3}/bin/python3 -c \"import socket, ssl; trust = open('/run/microvm-worker/trust.der', 'rb').read(); context = ssl.create_default_context(cadata=ssl.DER_cert_to_PEM_cert(trust)); connection = context.wrap_socket(socket.create_connection(('192.168.1.2', 7444), timeout=5), server_hostname='gateway'); connection.close()\"")
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/key")
    public_key = client.succeed("cat /root/key.pub").strip()
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/.ssh")
    gateway.succeed("printf '%s\\n' 'restrict,command=\"/etc/microvm-serve\" " + public_key + "' > /var/lib/telchar/.ssh/authorized_keys; chown telchar:telchar /var/lib/telchar/.ssh/authorized_keys")
    client.succeed("nix-instantiate /etc/microvm-build.nix --add-root /root/derivation")
    derivation = client.succeed("readlink -f /root/derivation").strip()
    client.succeed("NIX_SSHOPTS='-i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' nix --offline build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar@gateway aarch64-linux' " + derivation + "^* > /root/output")
    assert client.succeed("cat $(cat /root/output)") == "microvm-service-success\n"
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM shared_builds WHERE state = 'succeeded'\\\"\"").strip() == "1"
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
