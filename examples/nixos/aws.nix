# Composes an EC2 gateway with operator-owned Vault, PostgreSQL, and Nomad services.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  persistentStoreRoot = "/var/lib/telchar/nix-root";
  persistentStoreSocket = "/run/telchar-nix-daemon/socket";
  nomadEndpoint = config.services.telchar.deployment.nomadEndpoint;
  workerImage = config.services.telchar.deployment.workerImage;
  attachVolume = pkgs.writeShellApplication {
    name = "attach-telchar-volume";
    runtimeInputs = [
      pkgs.awscli2
      pkgs.jq
      pkgs.coreutils
    ];
    text = builtins.readFile ./attach-volume.sh;
  };
  attachPersistentVolume = pkgs.writeShellScript "attach-telchar-persistent-volume" ''
    set -euo pipefail

    metadata_token="$(${pkgs.curl}/bin/curl --fail --silent --show-error \
      --request PUT \
      --header 'X-aws-ec2-metadata-token-ttl-seconds: 21600' \
      http://169.254.169.254/latest/api/token)"
    metadata() {
      ${pkgs.curl}/bin/curl --fail --silent --show-error \
        --header "X-aws-ec2-metadata-token: $metadata_token" \
        "http://169.254.169.254/latest/meta-data/$1"
    }
    instance_id="$(metadata instance-id)"
    TELCHAR_PERSISTENT_VOLUME_ID="$(metadata tags/instance/TelcharPersistentVolumeId)"
    TELCHAR_DATABASE_SECRET_ARN="$(metadata tags/instance/TelcharDatabaseSecretArn)"
    TELCHAR_RDS_CA_BUNDLE_URL="$(metadata tags/instance/TelcharRdsCaBundleUrl)"
    TELCHAR_LIFECYCLE_HOOK_NAME="$(metadata tags/instance/TelcharLifecycleHookName)"
    TELCHAR_AUTOSCALING_GROUP_NAME="$(metadata tags/instance/TelcharAutoscalingGroupName)"
    availability_zone="$(${pkgs.curl}/bin/curl --fail --silent --show-error \
      --header "X-aws-ec2-metadata-token: $metadata_token" \
      http://169.254.169.254/latest/meta-data/placement/availability-zone)"
    region="''${availability_zone%?}"

    heartbeat() {
      ${pkgs.awscli2}/bin/aws autoscaling record-lifecycle-action-heartbeat \
        --region "$region" \
        --lifecycle-hook-name "$TELCHAR_LIFECYCLE_HOOK_NAME" \
        --auto-scaling-group-name "$TELCHAR_AUTOSCALING_GROUP_NAME" \
        --instance-id "$instance_id"
    }

    complete() {
      ${pkgs.awscli2}/bin/aws autoscaling complete-lifecycle-action \
        --region "$region" \
        --lifecycle-hook-name "$TELCHAR_LIFECYCLE_HOOK_NAME" \
        --auto-scaling-group-name "$TELCHAR_AUTOSCALING_GROUP_NAME" \
        --instance-id "$instance_id" \
        --lifecycle-action-result "$1"
    }

    ${attachVolume}/bin/attach-telchar-volume \
      "$region" "$TELCHAR_PERSISTENT_VOLUME_ID" "$instance_id" \
      "$TELCHAR_AUTOSCALING_GROUP_NAME" "$TELCHAR_LIFECYCLE_HOOK_NAME" 600

    volume_serial="''${TELCHAR_PERSISTENT_VOLUME_ID//-/}"
    device=""
    for _ in $(seq 1 12); do
      for candidate in /dev/nvme*n1; do
        [ -b "$candidate" ] || continue
        serial="$(${pkgs.util-linux}/bin/lsblk --noheadings --output SERIAL "$candidate" | ${pkgs.coreutils}/bin/tr -d '[:space:]-')"
        if [ "$serial" = "$volume_serial" ]; then
          device="$candidate"
          break
        fi
      done
      [ -n "$device" ] && break
      heartbeat
      sleep 5
    done
    [ -b "$device" ] || {
      echo "persistent volume device did not appear" >&2
      complete ABANDON
      exit 1
    }

    existing_label="$(${pkgs.util-linux}/bin/lsblk --noheadings --output LABEL "$device" | ${pkgs.coreutils}/bin/tr -d '[:space:]')"
    if [ -z "$existing_label" ]; then
      existing_type="$(${pkgs.util-linux}/bin/lsblk --noheadings --output FSTYPE "$device" | ${pkgs.coreutils}/bin/tr -d '[:space:]')"
      [ -z "$existing_type" ] || {
        echo "unrecognized unlabeled filesystem on persistent volume" >&2
        complete ABANDON
        exit 1
      }
      ${pkgs.e2fsprogs}/bin/mkfs.ext4 -L telchar "$device"
    elif [ "$existing_label" != "telchar" ]; then
      echo "persistent volume label is $existing_label, expected telchar" >&2
      complete ABANDON
      exit 1
    fi

  '';
  bootstrapPersistentVolume = pkgs.writeShellScript "bootstrap-telchar-persistent-volume" ''
    set -euo pipefail

    metadata_token="$(${pkgs.curl}/bin/curl --fail --silent --show-error \
      --request PUT \
      --header 'X-aws-ec2-metadata-token-ttl-seconds: 21600' \
      http://169.254.169.254/latest/api/token)"
    metadata() {
      ${pkgs.curl}/bin/curl --fail --silent --show-error \
        --header "X-aws-ec2-metadata-token: $metadata_token" \
        "http://169.254.169.254/latest/meta-data/$1"
    }
    TELCHAR_DATABASE_SECRET_ARN="$(metadata tags/instance/TelcharDatabaseSecretArn)"
    TELCHAR_RDS_CA_BUNDLE_URL="$(metadata tags/instance/TelcharRdsCaBundleUrl)"
    availability_zone="$(metadata placement/availability-zone)"
    region="''${availability_zone%?}"

    ${pkgs.coreutils}/bin/install -d -m 0700 -o telchar -g telchar \
      /var/lib/telchar/credentials \
      /var/lib/telchar/import \
      /var/lib/telchar/gc-roots \
      /var/lib/telchar/ssh
    ${pkgs.coreutils}/bin/install -m 0400 -o telchar -g telchar \
      <(${pkgs.awscli2}/bin/aws secretsmanager get-secret-value \
        --region "$region" \
        --secret-id "$TELCHAR_DATABASE_SECRET_ARN" \
        --query SecretString \
        --output text) \
      /var/lib/telchar/credentials/database-url
    ${pkgs.curl}/bin/curl --fail --silent --show-error --location \
      "$TELCHAR_RDS_CA_BUNDLE_URL" \
      --output /var/lib/telchar/credentials/rds-ca.pem
    ${pkgs.coreutils}/bin/chown telchar:telchar /var/lib/telchar/credentials/rds-ca.pem
    ${pkgs.coreutils}/bin/chmod 0400 /var/lib/telchar/credentials/rds-ca.pem

    if [ ! -s /var/lib/telchar/ssh/ssh_host_ed25519_key ]; then
      ${pkgs.openssh}/bin/ssh-keygen -q -t ed25519 -N "" -f /var/lib/telchar/ssh/ssh_host_ed25519_key
      ${pkgs.coreutils}/bin/chown telchar:telchar \
        /var/lib/telchar/ssh/ssh_host_ed25519_key \
        /var/lib/telchar/ssh/ssh_host_ed25519_key.pub
      ${pkgs.coreutils}/bin/chmod 0400 /var/lib/telchar/ssh/ssh_host_ed25519_key
      ${pkgs.coreutils}/bin/chmod 0444 /var/lib/telchar/ssh/ssh_host_ed25519_key.pub
    fi

    ${pkgs.coreutils}/bin/install -m 0400 -o telchar -g telchar /dev/stdin \
      /var/lib/telchar/ssh/authorized_principals <<'PRINCIPALS'
    nix-builder
    PRINCIPALS
  '';
  completeLaunchLifecycle = pkgs.writeShellScript "complete-telchar-launch-lifecycle" ''
    set -euo pipefail

    metadata_token="$(${pkgs.curl}/bin/curl --fail --silent --show-error \
      --request PUT \
      --header 'X-aws-ec2-metadata-token-ttl-seconds: 21600' \
      http://169.254.169.254/latest/api/token)"
    metadata() {
      ${pkgs.curl}/bin/curl --fail --silent --show-error \
        --header "X-aws-ec2-metadata-token: $metadata_token" \
        "http://169.254.169.254/latest/meta-data/$1"
    }
    instance_id="$(metadata instance-id)"
    lifecycle_hook_name="$(metadata tags/instance/TelcharLifecycleHookName)"
    autoscaling_group_name="$(metadata tags/instance/TelcharAutoscalingGroupName)"
    availability_zone="$(metadata placement/availability-zone)"
    region="''${availability_zone%?}"

    ${pkgs.awscli2}/bin/aws autoscaling complete-lifecycle-action \
      --region "$region" \
      --lifecycle-hook-name "$lifecycle_hook_name" \
      --auto-scaling-group-name "$autoscaling_group_name" \
      --instance-id "$instance_id" \
      --lifecycle-action-result CONTINUE
  '';
in
{
  options.services.telchar.deployment = {
    nomadEndpoint = lib.mkOption {
      type = lib.types.str;
      description = "External Nomad API endpoint used to submit generated builds.";
    };
    workerImage = lib.mkOption {
      type = lib.types.str;
      description = "Immutable Nomad worker image used by generated build allocations.";
    };
  };

  imports = [
    ./standalone.nix
    ./vault-aws.nix
  ];

  config = {
    networking = {
      hostName = "telchar";
      domain = "example.com";
    };

    fileSystems."/var/lib/telchar".options = [
      "_netdev"
      "x-systemd.device-timeout=13min"
      "x-systemd.requires=telchar-volume-attach.service"
      "x-systemd.after=telchar-volume-attach.service"
    ];

    services.telchar = {
      gatewayStore.uri = "unix://${persistentStoreSocket}";
      gatewayStore.directory = "${persistentStoreRoot}/nix/store";
      settings = {
        running_disconnect_policy = "detach-and-finish";
        output_retention_seconds = 86400;
        maximum_retained_input_bytes = 8589934592;
        ipc = {
          socket = "/run/telchar/daemon.sock";
          maximum_sessions = 64;
        };
        backends = {
          permit_wait_seconds = 30;
          nomad_callback = {
            public_url = "wss://telchar-callback.example.com/callback";
            maximum_connections = 64;
            maximum_header_bytes = 16384;
            maximum_body_bytes = 65536;
            authentication_request_timeout_seconds = 10;
            shutdown_drain_timeout_seconds = 30;
            maximum_jwks_bytes = 1048576;
            maximum_retained_nonces = 65536;
          };
          nomad = [
            {
              aws-spot = {
                system = "x86_64-linux";
                supported_features = [ ];
                maximum_concurrent_builds = 4;
                max_retries = 2;
                endpoint = nomadEndpoint;
                namespace = "telchar";
                node_pool = "aws-spot";
                token_file = "/run/telchar/credentials/nomad-token";
                driver = "docker";
                job_name_scope = "telchar-build";
                poll_interval_seconds = 1;
                runtime_limit_seconds = 3600;
                transfer_endpoint = "wss://telchar-callback.example.com/callback";
                constraints = [
                  {
                    attribute = "\${attr.cpu.arch}";
                    operator = "=";
                    value = "amd64";
                  }
                ];
                driver_config = {
                  image = workerImage;
                  force_pull = true;
                  mount = [
                    {
                      source = "/nix/var/nix/daemon-socket/socket";
                      target = "/nix/var/nix/daemon-socket/socket";
                      type = "bind";
                      readonly = false;
                    }
                  ];
                };
                resources = {
                  cpu_mhz = 2000;
                  memory_mb = 4096;
                  disk_mb = 16384;
                };
                priority = {
                  minimum = 40;
                  default = 50;
                  maximum = 60;
                };
                transfer_authentication = {
                  mode = "workload-identity";
                  issuer = nomadEndpoint;
                  verify_issuer = true;
                  jwks_url = "${nomadEndpoint}/.well-known/jwks.json";
                  audience = "telchar-transfer";
                };
                store = {
                  mode = "daemon";
                  uri = "unix:///nix/var/nix/daemon-socket/socket";
                };
                transfer_limits = {
                  maximum_manifest_paths = 65536;
                  maximum_manifest_bytes = 8388608;
                  maximum_input_nar_bytes = 17179869184;
                  maximum_total_input_bytes = 68719476736;
                  maximum_output_nar_bytes = 17179869184;
                  maximum_total_output_bytes = 68719476736;
                  maximum_frame_metadata_bytes = 1048576;
                  stream_buffer_bytes = 262144;
                  maximum_live_log_chunk_bytes = 65536;
                  live_log_queue_bytes = 1048576;
                  transfer_idle_timeout_seconds = 30;
                  setup_timeout_seconds = 300;
                  output_collection_timeout_seconds = 300;
                  maximum_connection_lifetime_seconds = 3600;
                  authentication_lifetime_seconds = 300;
                  clock_skew_seconds = 30;
                  nonce_retention_seconds = 600;
                  reconnect_timeout_seconds = 30;
                  maximum_diagnostic_bytes = 65536;
                };
              };
            }
          ];
        };
        scheduling.default = {
          maximum_queued_builds = 1024;
          maximum_active_builds = 64;
        };
      };
      callback = {
        enable = true;
        openFirewall = true;
      };
      vaultAwsAuth = {
        enable = true;
        address = "https://vault.example.com";
        role = "telchar-gateway";
        authMount = "aws";
        sshSignPath = "ssh-host-signer/sign/telchar-host";
        nomadSecretPath = "nomad/creds/telchar-builder";
        renewal.enable = true;
        cache = {
          enable = true;
          url = "https://attic.example.com/cache";
          username = "attic";
          tokenPath = "attic-tokens/data/telchar";
          publicKeyPath = "attic-tokens/data/public_key";
          publicKeyField = "cache";
          publicKeyName = "cache";
        };
      };
      database = {
        urlFile = "/var/lib/telchar/credentials/database-url";
        rootCertificateFile = "/var/lib/telchar/credentials/rds-ca.pem";
      };
      nomad = {
        tokenFile = "/var/lib/telchar/credentials/nomad-token";
        renewal = {
          enable = true;
          candidateFile = "/var/lib/telchar/credentials/nomad-token.candidate";
        };
      };
      ingress.openssh = {
        hostKeyFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key";
        hostCertificateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.pub";
        trustedUserCAKeysFile = "/var/lib/telchar/ssh/client-ca.pub";
        authorizedPrincipalsFile = "/var/lib/telchar/ssh/authorized_principals";
      };
      sshHostCertificateRenewal = {
        enable = true;
        candidateFile = "/var/lib/telchar/ssh/ssh_host_ed25519_key-cert.candidate.pub";
        expectedSigningCAFile = "/var/lib/telchar/ssh/host-ca.pub";
        expectedPrincipals = [ "telchar.example.com" ];
        minimumRemainingValiditySec = 300;
      };
    };

    systemd.sockets.telchar-nix-daemon = {
      description = "Telchar persistent Nix store socket";
      wantedBy = [ "multi-user.target" ];
      unitConfig.DefaultDependencies = false;
      before = [ "shutdown.target" ];
      conflicts = [ "shutdown.target" ];
      after = [ "var-lib-telchar.mount" ];
      requires = [ "var-lib-telchar.mount" ];
      listenStreams = [ persistentStoreSocket ];
      socketConfig = {
        DirectoryMode = "0755";
        SocketMode = "0660";
        SocketGroup = "telchar";
      };
    };

    systemd.services.telchar-volume-attach = {
      description = "Attach the Telchar persistent volume";
      before = [ "var-lib-telchar.mount" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = attachPersistentVolume;
        TimeoutStartSec = "12min";
      };
    };

    systemd.services.telchar-volume-bootstrap = {
      description = "Bootstrap the mounted Telchar persistent volume";
      wantedBy = [ "multi-user.target" ];
      after = [ "var-lib-telchar.mount" ];
      requires = [ "var-lib-telchar.mount" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = bootstrapPersistentVolume;
        TimeoutStartSec = "5min";
      };
    };

    systemd.services.telchar-vault-aws-auth = {
      after = [ "telchar-volume-bootstrap.service" ];
      requires = [ "telchar-volume-bootstrap.service" ];
    };

    systemd.services.telchar-credential-renewal = {
      after = [ "telchar-volume-bootstrap.service" ];
      requires = [ "telchar-volume-bootstrap.service" ];
    };

    systemd.services.telchar-launch-complete = {
      description = "Complete the Telchar launch lifecycle";
      wantedBy = [ "multi-user.target" ];
      after = [ "telchar-credential-renewal.service" ];
      requires = [ "telchar-credential-renewal.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = completeLaunchLifecycle;
      };
    };

    systemd.services.telchar-nix-daemon = {
      description = "Telchar persistent Nix store daemon";
      after = [
        "var-lib-telchar.mount"
        "telchar-vault-aws-auth.service"
      ];
      requires = [ "var-lib-telchar.mount" ];
      environment.NIX_USER_CONF_FILES = config.services.telchar.vaultAwsAuth.cache.configurationFile;
      serviceConfig = {
        ExecStartPre = "${pkgs.coreutils}/bin/install -d -m 0700 -o telchar -g telchar ${persistentStoreRoot}/nix/var/nix/gcroots/telchar";
        ExecStart = "${pkgs.nix}/bin/nix --extra-experimental-features nix-command --option require-sigs false daemon --store local?root=${persistentStoreRoot}";
        Restart = "on-failure";
      };
    };

    systemd.services.telchar = {
      after = [
        "telchar-credential-renewal.service"
        "telchar-nix-daemon.socket"
      ];
      requires = [
        "telchar-credential-renewal.service"
        "telchar-nix-daemon.socket"
      ];
    };

    systemd.services.telchar-nix-gc = {
      description = "Collect unreachable paths from the Telchar persistent Nix store";
      after = [ "telchar-nix-daemon.service" ];
      requires = [ "telchar-nix-daemon.service" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.nix}/bin/nix-collect-garbage --store unix://${persistentStoreSocket} --delete-older-than 30d";
      };
    };

    systemd.timers.telchar-nix-gc = {
      description = "Daily Telchar persistent Nix store garbage collection";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = "daily";
        Persistent = true;
        RandomizedDelaySec = "1h";
      };
    };
  };
}
