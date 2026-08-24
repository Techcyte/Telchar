# Generic Telchar gateway deployment for Nomad.
#
# This example uses:
#   - one singleton Telchar gateway;
#   - PostgreSQL for durable control-plane state;
#   - a sibling Nix daemon with a persistent store;
#   - Nomad workload identity for worker callback authentication;
#   - generated batch allocations using the Docker driver;
#   - a Nix daemon socket mounted on every eligible worker node.
#
# Replace every image reference, endpoint, storage path, namespace, and placement
# constraint for your cluster. Pin immutable image digests in production.
#
# Telchar does not terminate TLS. The callback listener below is plaintext
# WebSocket. Use cleartext WebSocket only on a trusted network. For encrypted
# WebSocket, put a reverse proxy or load balancer in front of port 7443 and set callback_public_url to its public
# URL. The proxy must preserve WebSocket upgrades and the
# telchar-nomad-transfer-v1 subprotocol, disable retries, and use an idle timeout
# greater than transfer_idle_timeout_seconds.

variable "namespace" {
  type        = string
  description = "Existing Nomad namespace for the gateway and generated build jobs"
  default     = "telchar"
}

variable "datacenters" {
  type        = list(string)
  description = "Nomad datacenters where the singleton gateway may run"
  default     = ["dc1"]
}

variable "gateway_image" {
  type        = string
  description = "Immutable telchar OCI image reference"
}

variable "nix_daemon_image" {
  type        = string
  description = "Immutable telchar-nix-daemon OCI image reference"
}

variable "worker_image" {
  type        = string
  description = "Immutable telchar-nomad-worker OCI image reference"
}

variable "nomad_api_endpoint" {
  type        = string
  description = "Nomad API URL reachable from the gateway and used as workload-identity issuer"
}

variable "callback_public_url" {
  type        = string
  description = "Stable WebSocket callback URL reachable from every build allocation"
}

variable "gateway_store_path" {
  type        = string
  description = "Persistent host directory mounted as /nix/store in the gateway Nix daemon"
  default     = "/srv/telchar/nix/store"
}

variable "gateway_state_path" {
  type        = string
  description = "Persistent host directory mounted as /nix/var/nix in the gateway Nix daemon"
  default     = "/srv/telchar/nix/var"
}

variable "worker_daemon_socket" {
  type        = string
  description = "Nix daemon socket available at the same host path on every eligible worker node"
  default     = "/nix/var/nix/daemon-socket/socket"
}

job "telchar" {
  type        = "service"
  namespace   = var.namespace
  datacenters = var.datacenters

  # Telchar is deliberately single-active. PostgreSQL leasing fences stale
  # processes, while count = 1 prevents intentional horizontal replication.
  # Keep max_parallel = 1 so deployments do not overlap singleton candidates.
  update {
    max_parallel      = 1
    stagger           = "30s"
    healthy_deadline  = "5m"
    progress_deadline = "10m"
  }

  # These are examples, not Telchar requirements. Match gateway placement to
  # nodes that provide gateway_store_path and gateway_state_path.
  constraint {
    attribute = "${attr.kernel.name}"
    value     = "linux"
  }

  group "gateway" {
    count = 1

    # Gateway restart/reschedule is safe because PostgreSQL and the gateway Nix
    # store are durable. This differs from generated execution jobs: execution
    # jobs explicitly disable restart and reschedule to prevent blind retries.
    restart {
      attempts = 10
      interval = "30m"
      delay    = "30s"
      mode     = "delay"
    }

    reschedule {
      delay          = "30s"
      delay_function = "exponential"
      max_delay      = "5m"
      unlimited      = true
    }

    network {
      mode = "bridge"

      port "callback" {
        # A static port makes a stable service or load-balancer target simple.
        # A dynamic port also works if service discovery or your proxy tracks it.
        static = 7443
        to     = 7443
      }

      # Optional stock-Nix SSH ingress commonly listens on 2222. Add this port
      # only when an SSH sidecar is enabled:
      #
      # port "ssh" {
      #   static = 2222
      #   to     = 2222
      # }
    }

    service {
      name     = "telchar-callback"
      provider = "nomad"
      port     = "callback"

      check {
        name     = "callback listener"
        type     = "tcp"
        interval = "30s"
        timeout  = "5s"
      }

      # Alternative: use Consul Connect to reach PostgreSQL. Add a connect
      # sidecar here, bind an upstream to 127.0.0.1:15432, and put that address
      # in database_url. Direct PostgreSQL networking works equally well.
    }

    # The official images run as 995:995. Host directories must already exist
    # and be writable by that identity. A privileged host setup job, CSI volume,
    # or operator provisioning may replace this prestart task.
    task "storage-permissions" {
      lifecycle {
        hook    = "prestart"
        sidecar = false
      }

      driver = "docker"
      user   = "0:0"

      config {
        image   = "busybox:1.37"
        command = "sh"
        args = [
          "-ec",
          "install -d -m 0700 -o 995 -g 995 /alloc/data/import /alloc/data/gc-roots /alloc/data/run",
        ]
      }

      resources {
        cpu    = 25
        memory = 16
      }
    }

    # Telchar must use a store distinct from the client store. This sibling
    # daemon owns gateway substitution, registration, validation, and retention.
    # Persist /nix/store and /nix/var/nix together; restoring only one corrupts
    # store authority.
    task "nix-daemon" {
      driver = "docker"
      user   = "995:995"

      config {
        image      = var.nix_daemon_image
        force_pull = true

        mount {
          type     = "bind"
          source   = var.gateway_store_path
          target   = "/nix/store"
          readonly = false
        }

        mount {
          type     = "bind"
          source   = var.gateway_state_path
          target   = "/nix/var/nix"
          readonly = false
        }
      }

      # OPERATOR POLICY: size for gateway-side substitution and dependency
      # realization. These are Nomad resources, not Telchar configuration defaults.
      resources {
        cpu    = 1000
        memory = 4096
      }
    }

    task "gateway" {
      driver = "docker"
      user   = "995:995"

      config {
        image      = var.gateway_image
        force_pull = true
        ports      = ["callback"]
        args = [
          "daemon",
          "--socket",
          "/alloc/data/run/daemon.sock",
          "--frontend-uid",
          "995",
        ]

        # The gateway accesses the sibling daemon only through its Unix socket.
        # It never manipulates the Nix store database directly.
        mount {
          type     = "bind"
          source   = "${var.gateway_state_path}/daemon-socket"
          target   = "/nix/var/nix/daemon-socket"
          readonly = false
        }
      }

      env {
        HOME                              = "/alloc/data"
        TMPDIR                            = "/alloc/data/import"
        TELCHAR_CONFIG                    = "/secrets/telchar.toml"
        TELCHAR_GATEWAY_STORE_URI         = "unix:///nix/var/nix/daemon-socket/socket"
        TELCHAR_GATEWAY_GC_ROOT_DIRECTORY = "/alloc/data/gc-roots"
        RUST_LOG                          = "info"

        # TELCHAR_GATEWAY_DISK_RESERVE_BYTES defaults to 10 GiB. Override only
        # from an operator capacity decision; low values risk exhausting the
        # gateway store during input admission or output import.
        #
        # Optional OTLP example:
        # OTEL_EXPORTER_OTLP_PROTOCOL = "http/protobuf"
        # OTEL_EXPORTER_OTLP_ENDPOINT = "https://otel.example.invalid"
      }

      # Create this Nomad Variable before registration:
      #
      #   nomad var put -namespace=telchar nomad/jobs/telchar/gateway \
      #     database_url='postgresql://telchar:password@postgres.example:5432/telchar' \
      #     nomad_token='replace-with-a-restricted-nomad-token'
      #
      # Prefer a dynamic Vault template or workload-identity-aware secret broker
      # when available. Files remain the authority consumed by Telchar; secrets
      # are not embedded in telchar.toml, command arguments, or job metadata.
      template {
        destination = "secrets/database-url"
        perms       = "0600"
        uid         = 995
        gid         = 995
        change_mode = "restart"
        data        = <<-EOH
{{ with nomadVar "nomad/jobs/telchar/gateway" }}{{ .database_url }}{{ end }}
EOH
      }

      template {
        destination   = "secrets/nomad-token"
        perms         = "0600"
        uid           = 995
        gid           = 995
        change_mode   = "signal"
        change_signal = "SIGHUP"
        data          = <<-EOH
{{ with nomadVar "nomad/jobs/telchar/gateway" }}{{ .nomad_token }}{{ end }}
EOH
      }

      template {
        destination   = "secrets/telchar.toml"
        perms         = "0600"
        uid           = 995
        gid           = 995
        change_mode   = "signal"
        change_signal = "SIGHUP"
        data          = <<-EOH
# Value labels used below:
#   REQUIRED        Telchar cannot construct this deployment without the value.
#   TELCHAR DEFAULT Repeated so operators can see and review the effective bound.
#   OPERATOR POLICY Example sizing or behavior; choose deliberately.

# OPERATOR POLICY: detached requests continue to completion. This prevents a
# transient client disconnect from cancelling already-dispatched execution.
running_disconnect_policy = "detach-and-finish"

# OPERATOR POLICY: defaults are 3600 seconds and i64::MAX bytes respectively.
# This example retains successful outputs for one day while capping retained
# admitted inputs at 8 GiB.
output_retention_seconds = 86400
maximum_retained_input_bytes = 8589934592

[database]
# REQUIRED: secret file rendered above.
url_file = "/secrets/database-url"
# TELCHAR DEFAULT: ownership renewal is 5 seconds and lease is 20 seconds.
# Omitted here. Keep lease at least three times renewal when overriding them.

[ipc]
# REQUIRED for this jobspec topology: gateway and ingress share this socket.
socket = "/alloc/data/run/daemon.sock"
# OPERATOR POLICY: Telchar default is 256 sessions.
maximum_sessions = 64

[backends.nomad_callback]
# TELCHAR DEFAULT: repeated to document the actual listener and protocol bounds.
bind = "0.0.0.0:7443"
maximum_connections = 64
maximum_header_bytes = 16384
maximum_body_bytes = 65536
authentication_request_timeout_seconds = 10
shutdown_drain_timeout_seconds = 30
maximum_jwks_bytes = 1048576
maximum_retained_nonces = 65536
# REQUIRED: unlike the loopback default, this URL must be reachable by workers.
public_url = "${var.callback_public_url}"

[scheduling.default]
# OPERATOR POLICY: Telchar defaults are 65536 queued and 65536 active builds.
# Keep active admission separate from backend maximum_concurrent_builds.
maximum_queued_builds = 1024
maximum_active_builds = 64

[backends]
# TELCHAR DEFAULT: repeated to make backend-capacity waiting explicit.
permit_wait_seconds = 30

# REQUIRED BACKEND: fields in this section define exact execution authority.
[[backends.nomad]]
name = "nomad-linux-amd64"
system = "x86_64-linux"
# Advertise profile selectors only when their operator policy below is enabled.
# A derivation requirement is not authorization; these mappings and bounds are.
supported_features = ["overflow-aws"]
maximum_concurrent_builds = 4
# OPERATOR POLICY: retries after the initial attempt. Use for infrastructure
# loss such as disappearing ephemeral nodes; builder and transfer failures do
# not retry. The original BuildDerivation timeout covers every attempt.
max_retries = 2
endpoint = "${var.nomad_api_endpoint}"
namespace = "${var.namespace}"
token_file = "/secrets/nomad-token"
driver = "docker"
job_name_scope = "telchar-build"
poll_interval_seconds = 1
runtime_limit_seconds = 3600

# Placement is operator authority. Add or remove constraints to match nodes
# where worker_daemon_socket exists. Never infer CPU or memory from closure size.
[[backends.nomad.constraints]]
attribute = "$${attr.cpu.arch}"
operator = "="
value = "amd64"

[backends.nomad.driver_config]
image = "${var.worker_image}"
force_pull = true

# Generated worker allocations mount the host Nix daemon. Docker must permit
# this bind mount, and the socket must exist at the same path on every eligible
# node. Alternatives are described in README.md.
[[backends.nomad.driver_config.mount]]
source = "${var.worker_daemon_socket}"
target = "/nix/var/nix/daemon-socket/socket"
type = "bind"
readonly = false

# OPERATOR POLICY: required explicit resources. Size from workload classes and
# measured demand, never from derivation or closure byte size.
[backends.nomad.resources]
cpu_mhz = 2000
memory_mb = 4096
disk_mb = 16384

# OPERATOR POLICY: default Nomad job priority. Priority controls preemption when
# enabled; it does not order pending jobs. Telchar does not accept a derivation-
# supplied numeric priority.
[backends.nomad.priority]
minimum = 40
default = 50
maximum = 60

# OPERATOR POLICY EXAMPLE: requiring the arbitrary Nix feature "overflow-aws"
# selects this profile. Base backend constraints above still apply. Replace or
# remove this mapping when your cluster has no such placement class.
[[backends.nomad.resource_profiles]]
name = "overflow"
required_feature = "overflow-aws"
cpu_mhz = 4000
memory_mb = 8192
disk_mb = 32768
priority_minimum = 50
priority_default = 60
priority_maximum = 70

[[backends.nomad.resource_profiles.constraints]]
attribute = "$${node.class}"
operator = "="
value = "aws-overflow"

# Profile constraints can select a hardware-capable class but do not reserve a
# device. GPU/device reservations are intentionally deferred.

# REQUIRED: every callback is authenticated even on a trusted private network.
[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "${var.nomad_api_endpoint}"
# verify_issuer defaults to false because Nomad omits iss unless oidc_issuer is
# configured. Set verify_issuer = true only when your JWTs contain this issuer.
verify_issuer = false
jwks_url = "${var.nomad_api_endpoint}/.well-known/jwks.json"
audience = "telchar-transfer"

# REQUIRED: allocation-side Nix authority.
[backends.nomad.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

# REQUIRED BOUNDS: Nomad backends intentionally have no implicit transfer-limit
# defaults. Values below are production-shaped examples; reduce or increase them
# only after considering memory, disk, expected closure, and expected output size.
[backends.nomad.transfer_limits]
maximum_manifest_paths = 65536
maximum_manifest_bytes = 8388608
maximum_input_nar_bytes = 17179869184
maximum_total_input_bytes = 68719476736
maximum_output_nar_bytes = 17179869184
maximum_total_output_bytes = 68719476736
maximum_frame_metadata_bytes = 1048576
stream_buffer_bytes = 262144
maximum_live_log_chunk_bytes = 65536
live_log_queue_bytes = 1048576
transfer_idle_timeout_seconds = 30
setup_timeout_seconds = 300
output_collection_timeout_seconds = 300
maximum_connection_lifetime_seconds = 3600
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536
EOH
      }

      # OPERATOR POLICY: gateway service resources, independent of generated
      # execution-worker resources above.
      resources {
        cpu    = 1000
        memory = 2048
      }
    }

    # Optional stock-Nix SSH ingress
    # --------------------------------
    # Telchar's IPC socket is the frontend boundary. An SSH sidecar can expose
    # ssh-ng without changing gateway or backend configuration. The packaged
    # image reads operator-managed files; it does not contact Vault.
    #
    # Static-key mode needs a host key and authorized_keys. Certificate mode
    # needs a host key, host certificate, and trusted client CA. In either mode:
    #   - start the packaged sshd as root; authenticated sessions use UID 995;
    #   - match gateway --frontend-uid to that session UID;
    #   - mount /alloc/data/run/daemon.sock from this group;
    #   - expose a stable TCP endpoint, commonly port 2222;
    #   - keep SSH identity files outside PostgreSQL and Telchar configuration.
    #
    # Optional Vault certificate sidecar. Vault is merely one credential
    # provider. This sidecar writes files atomically into the shared allocation
    # directory; telchar-ssh-ingress watches them and reloads sshd.
    #
    # task "ssh-certificates" {
    #   driver = "docker"
    #
    #   config {
    #     image   = "hashicorp/vault:replace-me"
    #     command = "/bin/sh"
    #     args    = ["local/ssh-certificates.sh"]
    #   }
    #
    #   vault {
    #     role         = "telchar-gateway"
    #     env          = false
    #     disable_file = false
    #   }
    #
    #   env {
    #     VAULT_ADDR       = "https://vault.example.invalid"
    #     VAULT_TOKEN_FILE = "/secrets/vault_token"
    #   }
    #
    #   template {
    #     destination = "local/ssh-certificates.sh"
    #     perms       = "0755"
    #     data        = <<'EOH'
    #!/bin/sh
    # set -eu
    # credential_directory=/alloc/data/ssh
    # host_public_key="$credential_directory/ssh_host_ed25519_key.pub"
    # while true; do
    #   token="$(cat "$VAULT_TOKEN_FILE")"
    #   temporary_ca="$(mktemp "$credential_directory/client-ca.XXXXXX")"
    #   temporary_certificate="$(mktemp "$credential_directory/host-certificate.XXXXXX")"
    #   VAULT_TOKEN="$token" vault read -field=public_key ssh-client/config/ca >"$temporary_ca"
    #   VAULT_TOKEN="$token" vault write -field=signed_key ssh-host/sign/telchar \
    #     public_key="$(cat "$host_public_key")" cert_type=host \
    #     valid_principals=telchar.example.invalid >"$temporary_certificate"
    #   chmod 0644 "$temporary_ca" "$temporary_certificate"
    #   mv -f "$temporary_ca" "$credential_directory/client-ca.pub"
    #   mv -f "$temporary_certificate" "$credential_directory/ssh_host_ed25519_key-cert.pub"
    #   sleep 43200
    # done
    # EOH
    #   }
    # }
    #
    # task "ssh-ingress" {
    #   driver = "docker"
    #
    #   config {
    #     image = "registry.example.invalid/telchar-ssh-ingress@sha256:replace-me"
    #     ports = ["ssh"]
    #   }
    #
    #   env {
    #     TELCHAR_IPC_SOCKET                    = "/alloc/data/run/daemon.sock"
    #     TELCHAR_SSH_HOST_IDENTITY_MODE        = "certificate"
    #     TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE = "certificate"
    #     TELCHAR_SSH_HOST_KEY_FILE              = "/alloc/data/ssh/ssh_host_ed25519_key"
    #     TELCHAR_SSH_HOST_CERTIFICATE_FILE      = "/alloc/data/ssh/ssh_host_ed25519_key-cert.pub"
    #     TELCHAR_SSH_CLIENT_CA_FILE             = "/alloc/data/ssh/client-ca.pub"
    #   }
    # }
  }
}
