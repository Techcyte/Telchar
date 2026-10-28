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
  nodes.gateway = { config, ... }: {
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
      export TELCHAR_CONFIG=${config.systemd.services.telchar.environment.TELCHAR_CONFIG}
      export TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock
      export TELCHAR_AUTHENTICATED_KEY="$1"
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
    systemd.services.microvm-credentials = {
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        RuntimeDirectory = "microvm-aws";
        RuntimeDirectoryMode = "0700";
        UMask = "0077";
        ExecStart = pkgs.writeShellScript "microvm-credentials" ''
          printf '%s\n' '[gateway]' \
            'aws_access_key_id = fixture-key' \
            'aws_secret_access_key = fixture-secret' \
            'aws_session_token = fixture-token' > /run/microvm-aws/credentials
        '';
      };
    };
    services.telchar = {
      enable = true;
      credentials = [{ name = "aws-credentials"; source = "/run/microvm-aws/credentials"; }];
      package = daemon;
      frontendUid = 995;
      database.url = "host=/run/postgresql user=telchar dbname=telchar";
      settings = {
        identity.credentials = {
          "ssh-pubkey:SHA256:microvm-alice" = { audit_subject = "alice"; quota_subject = "alice"; };
          "ssh-pubkey:SHA256:microvm-bob" = { audit_subject = "bob"; quota_subject = "bob"; };
        };
        scheduling.default = {
          maximum_queued_builds = 1;
          maximum_active_builds = 1;
        };
        backends = {
          microvm_bind = "127.0.0.1:7443";
          microvm = [{
            name = "build";
            system = "aarch64-linux";
            account_id = "116392764028";
            region = "us-west-2";
            image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder";
            image_version = "1";
            vpc_egress_connector_arn = "arn:aws:lambda:us-west-2:116392764028:network-connector:builder";
            maximum_concurrent_builds = 1;
            maximum_duration_seconds = 60;
            execution_timeout_seconds = 50;
            admission_timeout_seconds = 20;
            provider_timeout_seconds = 2;
            cleanup_interval_seconds = 5;
            callback_url = "wss://gateway:7444/microvm";
            callback_address = "192.168.1.2:7444";
            staging_directory = "/var/lib/telchar";
            maximum_paths = 64;
            maximum_nar_bytes = 67108864;
            maximum_total_bytes = 134217728;
            metadata_bytes = 16384;
            chunk_bytes = 262144;
          }];
        };
      };
      environment = {
        AWS_EC2_METADATA_DISABLED = "true";
        AWS_SHARED_CREDENTIALS_FILE = "%d/aws-credentials";
        AWS_CONFIG_FILE = "/dev/null";
        AWS_PROFILE = "gateway";
        TELCHAR_TEST_MICROVM_ENDPOINT = "http://127.0.0.1:9876";
      };
    };
    systemd.services.telchar = {
      after = [ "postgresql-setup.service" "microvm-provider.service" "microvm-credentials.service" ];
      requires = [ "postgresql-setup.service" "microvm-provider.service" "microvm-credentials.service" ];
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
      { name ? "microvm-service-output", delay ? 0, payloadBytes ? 0 }:
      derivation {
        inherit name;
        system = "aarch64-linux";
        builder = "${pkgs.bash}/bin/bash";
        builderPackage = builtins.storePath "${pkgs.bash}";
        sleepPackage = builtins.storePath "${pkgs.coreutils}";
        args = [ "-c" "test $EUID != 0 && ${pkgs.coreutils}/bin/sleep ''${builtins.toString delay} && printf 'microvm-service-success\\n' > $out && ${pkgs.coreutils}/bin/head -c ''${builtins.toString payloadBytes} /dev/zero >> $out" ];
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
    client.succeed("ssh-keygen -q -t ed25519 -N \"\" -f /root/bob")
    bob_key = client.succeed("cat /root/bob.pub").strip()
    gateway.succeed("install -d -m 700 -o telchar -g telchar /var/lib/telchar/.ssh")
    gateway.succeed("printf '%s\\n' 'restrict,command=\"/etc/microvm-serve SHA256:microvm-alice\" " + public_key + "' > /var/lib/telchar/.ssh/authorized_keys; chown telchar:telchar /var/lib/telchar/.ssh/authorized_keys")
    gateway.succeed("printf '%s\\n' 'restrict,command=\"/etc/microvm-serve SHA256:microvm-bob\" " + bob_key + "' >> /var/lib/telchar/.ssh/authorized_keys")
    gateway.succeed("systemd-run --unit=microvm-staging ${pkgs.python3}/bin/python3 ${../../../tests/nixos/microvm-staging.py} $(systemctl show telchar.service -p MainPID --value)")
    gateway.wait_until_succeeds("test -s /run/microvm-staging.jsonl", timeout=10)
    client.succeed("nix-instantiate /etc/microvm-build.nix --arg delay 15 --arg payloadBytes 16777216 --add-root /root/derivation")
    derivation = client.succeed("readlink -f /root/derivation").strip()
    def submit(name, path, key="key"):
        command = "PATH=/run/current-system/sw/bin NIX_SSHOPTS='-i /root/" + key + " -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' nix --offline build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar@gateway aarch64-linux - 4' " + path + "^* > /root/" + name + "-output 2>/root/" + name + "-stderr; echo $? > /root/" + name + "-status"
        import shlex
        client.succeed("systemd-run --unit=" + name + " /bin/sh -c " + shlex.quote(command))
    submit("first", derivation)
    try:
        gateway.wait_until_succeeds("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT count(*) FROM microvm_launches WHERE session_consumed_at IS NOT NULL'\" | grep -qx 1", timeout=20)
    except Exception:
        print(client.succeed("cat /root/first-stderr; cat /root/first-status 2>/dev/null || true"))
        raise
    client.succeed("nix-instantiate /etc/microvm-build.nix --argstr name alice-queued --add-root /root/queued")
    queued = client.succeed("readlink -f /root/queued").strip()
    submit("queued", queued)
    try:
        gateway.wait_until_succeeds("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM shared_builds WHERE state = 'claimed' AND quota_subject = 'alice'\\\"\" | grep -qx 1", timeout=10)
    except Exception:
        print(client.succeed("cat /root/queued-stderr; cat /root/queued-status 2>/dev/null || true"))
        print(gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT quota_subject,state FROM shared_builds ORDER BY queue_position'\""))
        raise
    client.succeed("nix-instantiate /etc/microvm-build.nix --argstr name alice-overflow --add-root /root/overflow")
    overflow = client.succeed("readlink -f /root/overflow").strip()
    submit("overflow", overflow)
    client.wait_until_succeeds("test -f /root/overflow-status", timeout=10)
    assert client.succeed("cat /root/overflow-status").strip() != "0"
    overflow_error = client.succeed("cat /root/overflow-stderr")
    assert "shared build scheduling failed" in overflow_error, overflow_error
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT count(*) FROM microvm_launches'\"").strip() == "1"
    client.succeed("nix-instantiate /etc/microvm-build.nix --argstr name bob-overlapping --add-root /root/second")
    second = client.succeed("readlink -f /root/second").strip()
    assert len({derivation, queued, second}) == 3
    submit("second", second, "bob")
    gateway.wait_until_succeeds("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM shared_builds WHERE state = 'running' AND quota_subject = 'bob'\\\"\" | grep -qx 1", timeout=10)
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM shared_builds WHERE state = 'claimed' AND quota_subject = 'alice'\\\"\"").strip() == "1"
    client.wait_until_succeeds("test -f /root/first-status -a -f /root/queued-status -a -f /root/second-status", timeout=90)
    print(gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT provider_id, reservation_released_at IS NULL, termination_observed_at IS NULL FROM microvm_launches'\""))
    assert client.succeed("cat /root/first-status /root/queued-status /root/second-status").strip().splitlines() == ["0", "0", "0"], client.succeed("cat /root/first-stderr /root/queued-stderr /root/second-stderr")
    assert client.succeed("cat $(cat /root/queued-output)") == "microvm-service-success\n"
    client.succeed("cp /root/first-output /root/output")
    assert client.succeed("wc -c < $(cat /root/output)").strip() == "16777240"
    assert client.succeed("head -c 24 $(cat /root/output)") == "microvm-service-success\n"
    gateway.wait_until_succeeds("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT count(*) FROM microvm_launches WHERE reservation_released_at IS NULL'\" | grep -qx 0", timeout=90)
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc 'SELECT count(*) FROM microvm_launches WHERE termination_observed_at IS NOT NULL'\"").strip() == "3"
    assert client.succeed("cat $(cat /root/second-output)") == "microvm-service-success\n"
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM microvm_launches a JOIN microvm_launches b ON a.created_at < b.created_at WHERE a.reservation_released_at > b.created_at\\\"\"").strip() == "0"
    alice_order = gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT a.derivation_path FROM microvm_launches l JOIN shared_build_attempts a USING (attempt_id) JOIN shared_builds b USING (derivation_path) WHERE b.quota_subject = 'alice' ORDER BY l.created_at\\\"\"").strip().splitlines()
    assert alice_order == [derivation, queued], alice_order
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT count(*) FROM shared_builds WHERE state = 'succeeded'\\\"\"").strip() == "3"
    assert gateway.succeed("su -s /bin/sh postgres -c \"psql -d telchar -Atc \\\"SELECT quota_subject FROM shared_builds WHERE state = 'succeeded' ORDER BY quota_subject\\\"\"").strip().splitlines() == ["alice", "alice", "bob"]
    gateway.succeed("touch /run/microvm-staging-stop")
    gateway.wait_until_succeeds("test $(systemctl show microvm-staging.service -p ExecMainStatus --value) = 0 && ! systemctl is-active --quiet microvm-staging.service", timeout=10)
    gateway.copy_from_machine("/run/microvm-staging.jsonl")
    import json
    with open(os.environ["out"] + "/microvm-staging.jsonl") as samples_file:
        staging_samples = [json.loads(line) for line in samples_file]
    assert max(sample["logical_bytes"] for sample in staging_samples) - staging_samples[0]["logical_bytes"] >= 16777216, staging_samples
    print("GATEWAY_STAGING_SAMPLED_LOGICAL_MAX=" + str(max(sample["logical_bytes"] for sample in staging_samples)))
    print("GATEWAY_STAGING_SAMPLED_ALLOCATED_MAX=" + str(max(sample["allocated_bytes"] for sample in staging_samples)))
    gateway.succeed("systemctl stop telchar.service")
    gateway.wait_for_closed_port(7443)
    gateway.succeed("test ! -e /run/telchar/daemon.sock")
    gateway.succeed("systemctl start telchar.service")
    gateway.wait_for_open_port(7443)
    gateway.succeed("systemctl stop telchar.service")
    gateway.wait_for_closed_port(7443)
    service_journal = gateway.succeed("journalctl -u telchar.service -u microvm-provider.service -u postgresql.service --no-pager")
    errors = [line for line in service_journal.splitlines() if " ERROR " in line]
    assert len(errors) == 1 and 'rejection="shared-build-scheduling"' in errors[0], service_journal
    assert "diagnostic=shared build quota exceeded" in service_journal, service_journal
    assert not any(marker in service_journal for marker in ("Traceback", "FATAL:")), service_journal
  '';
}
