# Defines executable OCI archive, runtime, deployment, recovery, and bounded-load checks.
{
  pkgs,
  system,
  telcharImage,
  nomadWorkerImage,
}:
{
  nixos-oci-gateway =
    let
      classic = pkgs.writeText "telchar-oci-classic.nix" ''
        derivation {
          name = "telchar-oci-classic";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-oci-classic > $out" ];
        }
      '';
      flat = pkgs.writeText "telchar-oci-fixed-flat.nix" ''
        derivation {
          name = "telchar-oci-fixed-flat";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "printf telchar-oci-fixed-flat > $out" ];
          outputHashMode = "flat";
          outputHashAlgo = "sha256";
          outputHash = "0eb6746201147efa7010dfb19b3bce80b1709904b80414c399fcd2d945f97c96";
        }
      '';
      shared = pkgs.writeText "telchar-oci-shared.nix" ''
        derivation {
          name = "telchar-oci-shared";
          system = builtins.currentSystem;
          builder = builtins.storePath "${pkgs.runtimeShell}";
          args = [ "-c" "sleep 1; printf telchar-oci-shared > $out" ];
        }
      '';
      soak = builtins.genList (
        index:
        pkgs.writeText "telchar-oci-soak-${toString index}.nix" ''
          derivation {
            name = "telchar-oci-soak-${toString index}";
            system = builtins.currentSystem;
            builder = builtins.storePath "${pkgs.runtimeShell}";
            args = [ "-c" "printf telchar-oci-soak-${toString index} > $out" ];
          }
        ''
      ) 6;
    in
    pkgs.testers.nixosTest {
      name = "telchar-nixos-oci-gateway";
      nodes = {
        gateway =
          { pkgs, ... }:
          {
            networking.firewall.enable = false;
            virtualisation.docker.enable = true;
            services.postgresql = {
              enable = true;
              package = pkgs.postgresql;
              enableTCPIP = true;
              authentication = pkgs.lib.mkForce ''
                host telchar telchar 127.0.0.1/32 trust
                host telchar telchar ::1/128 trust
                local all all trust
              '';
              ensureDatabases = [ "telchar" ];
              ensureUsers = [
                {
                  name = "telchar";
                  ensureDBOwnership = true;
                }
              ];
            };
            users.groups.telchar = { };
            users.users.telchar = {
              isSystemUser = true;
              uid = 995;
              group = "telchar";
              home = "/var/lib/telchar-oci/ingress";
              createHome = true;
              shell = "${pkgs.bashInteractive}/bin/bash";
            };
            security.sudo.extraRules = [
              {
                users = [ "telchar" ];
                commands = [
                  {
                    command = "${pkgs.docker}/bin/docker";
                    options = [ "NOPASSWD" ];
                  }
                ];
              }
            ];
            services.openssh = {
              enable = true;
              settings = {
                PasswordAuthentication = false;
                KbdInteractiveAuthentication = false;
                PermitTTY = false;
                AllowTcpForwarding = false;
                AllowAgentForwarding = false;
                X11Forwarding = false;
                PermitUserEnvironment = false;
              };
              extraConfig = ''
                Match User telchar
                  AuthorizedKeysFile /var/lib/telchar-oci/ingress/.ssh/authorized_keys
                  ForceCommand /etc/telchar-oci-forced-command
                  DisableForwarding yes
                  PermitTTY no
              '';
            };
            environment.etc."telchar-oci-forced-command" = {
              mode = "0555";
              text = ''
                #!${pkgs.runtimeShell}
                set -eu
                exec /run/wrappers/bin/sudo -n ${pkgs.docker}/bin/docker exec -i --user 995:995 \
                  --env TELCHAR_IPC_SOCKET=/run/telchar/daemon.sock \
                  --env TELCHAR_AUTHENTICATED_KEY=SHA256:oci-fixture \
                  telchar-daemon /bin/telchar serve-stdio
              '';
            };
            nix.settings.trusted-users = [
              "root"
              "telchar"
            ];
            environment.systemPackages = [
              pkgs.docker
              pkgs.openssh
              pkgs.postgresql
              pkgs.bash
            ];
            system.stateVersion = "26.05";
          };
        client =
          { pkgs, ... }:
          {
            networking.firewall.enable = false;
            environment.systemPackages = [
              pkgs.nix
              pkgs.openssh
            ];
            system.stateVersion = "26.05";
          };
      };
      testScript = ''
        start_all()
        gateway.wait_for_unit("docker.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.wait_for_unit("nix-daemon.socket")
        gateway.succeed("docker load < ${telcharImage}")
        gateway.succeed("install -d -m 0700 -o 995 -g 995 /etc/telchar-oci /run/telchar-oci /var/lib/telchar/import /var/lib/telchar/gc-roots /var/lib/telchar-oci/ingress/.ssh")
        gateway.succeed("cat > /etc/telchar-oci/telchar.toml <<'EOF'\nrunning_disconnect_policy = \"detach-and-finish\"\n\n[backends.local]\nname = \"local\"\nsystem = \"${system}\"\nmaximum_concurrent_builds = 1\nEOF\nchmod 0444 /etc/telchar-oci/telchar.toml")
        client.succeed("mkdir -p /root/.ssh && ssh-keygen -q -t ed25519 -N \"\" -f /root/.ssh/telchar")
        public_key = client.succeed("cat /root/.ssh/telchar.pub").strip()
        gateway.succeed("printf '%s\n' '" + public_key + "' > /var/lib/telchar-oci/ingress/.ssh/authorized_keys && chown -R 995:995 /var/lib/telchar-oci/ingress/.ssh && chmod 0700 /var/lib/telchar-oci/ingress/.ssh && chmod 0600 /var/lib/telchar-oci/ingress/.ssh/authorized_keys")
        gateway.succeed("docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.wait_for_unit("sshd.service")
        client.succeed("ssh-keyscan gateway > /root/.ssh/known_hosts 2>/dev/null")
        ssh_options = "-i /root/.ssh/telchar -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/root/.ssh/known_hosts"
        client.succeed("HOME=/root NIX_SSHOPTS='" + ssh_options + "' timeout 30 nix --extra-experimental-features nix-command --store ssh-ng://telchar@gateway store info > /tmp/oci-store-info 2>&1 || { cat /tmp/oci-store-info >&2; exit 1; }")
        client.succeed("grep -q 'Version: telchar' /tmp/oci-store-info")
        for expression, expected in [("${classic}", "telchar-oci-classic"), ("${flat}", "telchar-oci-fixed-flat")]:
            client.succeed("cp " + expression + " /tmp/oci-build.nix")
            derivation_path = client.succeed("nix-instantiate /tmp/oci-build.nix").strip()
            derivation_export = client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
            gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
            command = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='" + ssh_options + "' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar@gateway ${system}' '" + derivation_path + "^*'"
            output_path = client.succeed(command).strip()
            gateway.succeed("test \"$(cat '" + output_path + "')\" = " + expected)
            gateway.succeed("find /var/lib/telchar/gc-roots -type l -lname '" + output_path + "' | grep -q .")
        shared_derivation = client.succeed("nix-instantiate ${shared}").strip()
        shared_export = client.succeed("nix-store --export '" + shared_derivation + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
        gateway.succeed("printf '%s' '" + shared_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
        shared_command = "HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='" + ssh_options + "' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar@gateway ${system}' '" + shared_derivation + "^*'"
        client.succeed("set -m; " + shared_command + " > /tmp/oci-shared-a & first=$!; " + shared_command + " > /tmp/oci-shared-b & second=$!; wait $first; wait $second")
        client.succeed("cmp /tmp/oci-shared-a /tmp/oci-shared-b && test \"$(cat $(cat /tmp/oci-shared-a))\" = telchar-oci-shared")
        soak_derivations = []
        for index, expression in enumerate(${builtins.toJSON soak}):
            derivation_path = client.succeed("nix-instantiate " + expression).strip()
            derivation_export = client.succeed("nix-store --export '" + derivation_path + "' | ${pkgs.coreutils}/bin/base64 -w0").strip()
            gateway.succeed("printf '%s' '" + derivation_export + "' | ${pkgs.coreutils}/bin/base64 -d | nix-store --import >/dev/null")
            soak_derivations.append((index, derivation_path))
        soak_commands = []
        soak_waits = []
        for index, derivation_path in soak_derivations:
            soak_commands.append("HOME=/root NIX_CONFIG='substituters =' NIX_SSHOPTS='" + ssh_options + "' nix --extra-experimental-features nix-command build --no-link --print-out-paths --max-jobs 0 --builders 'ssh-ng://telchar@gateway ${system}' '" + derivation_path + "^*' > /tmp/oci-soak-" + str(index) + " & pid" + str(index) + "=$!")
            soak_waits.append("wait $pid" + str(index))
        client.succeed("set -m; " + "; ".join(soak_commands + soak_waits))
        for index, _ in soak_derivations:
            client.succeed("test \"$(cat $(cat /tmp/oci-soak-" + str(index) + "))\" = telchar-oci-soak-" + str(index))
        gateway.succeed("sudo -u postgres psql -d telchar -Atc 'select max(version) from telchar_schema_migrations' | grep -qx 15")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc \"select count(*) from shared_builds where state = 'succeeded'\") -eq 9")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc \"select count(*) from shared_build_attempts where state in ('running', 'collecting')\") -eq 0")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc \"select count(*) from store_leases where owner_kind = 'request' and purpose in ('derivation', 'input') and state = 'active'\") -eq 0")
        gateway.succeed("docker stop --time 10 telchar-daemon && test \"$(docker inspect -f '{{.State.ExitCode}}' telchar-daemon)\" -eq 0 && find /var/lib/telchar/gc-roots -type l -delete")
        gateway.succeed("docker rm telchar-daemon >/dev/null && docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc \"select count(*) from shared_builds where state = 'succeeded'\") -eq 9")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        client.succeed("nix-store --delete '" + output_path + "'")
        gateway.succeed("systemctl stop nix-daemon.socket nix-daemon.service")
        client.fail(command)
        gateway.succeed("docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true")
        gateway.succeed("systemctl start nix-daemon.socket")
        gateway.wait_for_unit("nix-daemon.socket")
        gateway.succeed("docker rm -f telchar-daemon >/dev/null && docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        output_path = client.succeed(command).strip()
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        gateway.succeed("docker kill --signal KILL telchar-daemon >/dev/null && test \"$(docker inspect -f '{{.State.ExitCode}}' telchar-daemon)\" -eq 137")
        gateway.succeed("docker rm telchar-daemon >/dev/null && docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TELCHAR_SINGLETON_CHECK_INTERVAL_MS=100 -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.succeed("systemctl stop postgresql.service")
        gateway.wait_until_succeeds("docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx false", timeout=30)
        gateway.succeed("test ! -S /run/telchar-oci/daemon.sock && test \"$(docker inspect -f '{{.State.ExitCode}}' telchar-daemon)\" -eq 1")
        gateway.succeed("docker logs telchar-daemon > /tmp/telchar-daemon.log 2>&1 && grep -q database.singleton_ownership.lost /tmp/telchar-daemon.log")
        gateway.succeed("systemctl start postgresql.service")
        gateway.wait_for_unit("postgresql.service")
        gateway.succeed("docker rm telchar-daemon >/dev/null && docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        client.succeed("nix-store --delete '" + output_path + "'")
        output_path = client.succeed(command).strip()
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        image_id = gateway.succeed("docker image inspect telchar:latest --format '{{.Id}}'").strip()
        gateway.succeed("sudo -u postgres pg_dump -Fc -f /tmp/telchar-before-redeployment.dump telchar")
        gateway.succeed("docker stop --time 10 telchar-daemon >/dev/null && docker rm telchar-daemon >/dev/null && docker image rm telchar:latest >/dev/null && docker load < ${telcharImage} >/dev/null")
        assert gateway.succeed("docker image inspect telchar:latest --format '{{.Id}}'").strip() == image_id
        gateway.succeed("docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.succeed("docker logs telchar-daemon > /tmp/telchar-redeployment.log 2>&1 && grep -q 'previously_applied_count=15 applied_this_run_count=0 resulting_schema_version=15' /tmp/telchar-redeployment.log")
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        gateway.succeed("docker stop --time 10 telchar-daemon >/dev/null && docker rm telchar-daemon >/dev/null && sudo -u postgres psql -d telchar -c \"insert into telchar_schema_migrations (version, name, checksum) values (16, 'unsupported-future', decode(repeat('00', 32), 'hex'))\" >/dev/null")
        gateway.succeed("docker run -d --name telchar-unsupported --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("docker inspect -f '{{.State.Running}}' telchar-unsupported | grep -qx false", timeout=30)
        gateway.succeed("test ! -S /run/telchar-oci/daemon.sock && test \"$(docker inspect -f '{{.State.ExitCode}}' telchar-unsupported)\" -eq 1 && docker logs telchar-unsupported > /tmp/telchar-unsupported.log 2>&1 && grep -q database.migration.failed /tmp/telchar-unsupported.log")
        gateway.succeed("docker rm telchar-unsupported >/dev/null && sudo -u postgres dropdb --force telchar && sudo -u postgres createdb -O telchar telchar && sudo -u postgres pg_restore -d telchar /tmp/telchar-before-redeployment.dump")
        gateway.succeed("docker run -d --name telchar-daemon --network host --user 995:995 -e HOME=/var/lib/telchar -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_GATEWAY_STORE_URI=unix:///nix/var/nix/daemon-socket/socket -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-oci:/etc/telchar:ro -v /run/telchar-oci:/run/telchar -v /var/lib/telchar/import:/var/lib/telchar/import -v /var/lib/telchar/gc-roots:/var/lib/telchar/gc-roots -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro -v /nix/var/nix/daemon-socket/socket:/nix/var/nix/daemon-socket/socket telchar:latest daemon --socket /run/telchar/daemon.sock --frontend-uid 995")
        gateway.wait_until_succeeds("test -S /run/telchar-oci/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-daemon | grep -qx true", timeout=30)
        gateway.succeed("sudo -u postgres psql -d telchar -Atc 'select max(version) from telchar_schema_migrations' | grep -qx 15 && test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
        client.succeed("nix-store --delete '" + output_path + "'")
        output_path = client.succeed(command).strip()
        gateway.succeed("test $(sudo -u postgres psql -d telchar -Atc 'select count(*) from shared_build_attempts') -eq 9")
      '';
    };

  nixos-oci-runtime = pkgs.testers.nixosTest {
    name = "telchar-nixos-oci-runtime";
    nodes.runtime =
      { pkgs, ... }:
      {
        networking.firewall.enable = false;
        virtualisation.docker.enable = true;
        services.postgresql = {
          enable = true;
          package = pkgs.postgresql;
          enableTCPIP = true;
          authentication = pkgs.lib.mkForce ''
            host telchar telchar 127.0.0.1/32 trust
            host telchar telchar ::1/128 trust
            local all all trust
          '';
          ensureDatabases = [ "telchar" ];
          ensureUsers = [
            {
              name = "telchar";
              ensureDBOwnership = true;
            }
          ];
        };
        environment.systemPackages = [
          pkgs.docker
          pkgs.postgresql
        ];
        system.stateVersion = "26.05";
      };
    testScript = ''
      start_all()
      runtime.wait_for_unit("docker.service")
      runtime.wait_for_unit("postgresql.service")
      runtime.succeed("docker load < ${telcharImage}")
      runtime.succeed("docker load < ${nomadWorkerImage}")
      runtime.succeed("docker image inspect telchar:latest --format '{{json .Config.Entrypoint}} {{json .Config.Cmd}}' | grep -Fx '[\"/bin/telchar\"] [\"daemon\",\"--socket\",\"/run/telchar/daemon.sock\",\"--frontend-uid\",\"0\"]'")
      runtime.succeed("docker image inspect telchar-nomad-worker:latest --format '{{json .Config.Entrypoint}} {{json .Config.Cmd}}' | grep -Fx '[\"/bin/telchar-nomad-worker\"] null'")
      runtime.succeed("test \"$(docker run --rm --entrypoint /bin/telchar telchar:latest)\" = 'Nix worker protocol'")
      runtime.succeed("test \"$(docker run --rm --entrypoint /bin/ssh telchar:latest -V 2>&1; echo $?)\" != 127")
      runtime.succeed("set +e; docker run --rm telchar-nomad-worker:latest >/tmp/worker.out 2>/tmp/worker.err; status=$?; set -e; test $status -eq 1")
      runtime.succeed("grep -Fx 'telchar-nomad-worker: worker environment is incomplete' /tmp/worker.err")
      runtime.succeed("set +e; docker run --rm -e TELCHAR_DATABASE_URL=postgresql://unreachable/telchar -e TELCHAR_CONFIG=/missing/telchar.toml telchar:latest >/tmp/gateway.out 2>/tmp/gateway.err; status=$?; set -e; test $status -eq 1")
      runtime.succeed("grep -Fx 'telchar: database migration failed' /tmp/gateway.err")
      runtime.succeed("mkdir -p /etc/telchar-container /run/telchar-container /var/lib/telchar-container/import /var/lib/telchar-container/gc-roots && chmod 0700 /run/telchar-container")
      runtime.succeed("cat > /etc/telchar-container/telchar.toml <<'EOF'\nrunning_disconnect_policy = \"detach-and-finish\"\n\n[backends.local]\nname = \"local\"\nsystem = \"${system}\"\nmaximum_concurrent_builds = 1\nEOF\nchmod 0444 /etc/telchar-container/telchar.toml\nchown -R 0:0 /etc/telchar-container /run/telchar-container /var/lib/telchar-container")
      runtime.succeed("docker run -d --name telchar-runtime --network host --user 0:0 -e TELCHAR_CONFIG=/etc/telchar/telchar.toml -e TELCHAR_DATABASE_URL=postgresql://telchar@localhost/telchar -e TELCHAR_GATEWAY_DISK_RESERVE_BYTES=1048576 -e TELCHAR_TEST_STORE_RETENTION=1 -e TELCHAR_TEST_BUILD_HELPER=/bin/true -e TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=/var/lib/telchar/gc-roots -e TMPDIR=/var/lib/telchar/import -v /etc/telchar-container:/etc/telchar:ro -v /run/telchar-container:/run/telchar -v /var/lib/telchar-container:/var/lib/telchar telchar:latest")
      runtime.wait_until_succeeds("test -S /run/telchar-container/daemon.sock && docker inspect -f '{{.State.Running}}' telchar-runtime | grep -qx true", timeout=30)
      runtime.succeed("sudo -u postgres psql -d telchar -Atc 'select max(version) from telchar_schema_migrations' | grep -qx 15")
      runtime.succeed("docker stop --time 10 telchar-runtime")
      runtime.succeed("test \"$(docker inspect -f '{{.State.ExitCode}}' telchar-runtime)\" -eq 0")
    '';
  };
}
