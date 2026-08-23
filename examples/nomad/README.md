# Nomad deployment example

[`telchar.nomad.hcl`](telchar.nomad.hcl) is a generic starting point for a production Telchar gateway using Nomad. It deliberately contains no organization-specific domains, credentials, storage products, placement classes, or service names.

The example is verbose because the important deployment boundaries are operator policy, not safe Telchar defaults.

## Topology

```text
stock Nix client
  → optional restricted OpenSSH ingress
  → Telchar IPC socket
  → Telchar gateway
      ├── PostgreSQL durable control plane
      ├── dedicated persistent gateway Nix daemon
      └── Nomad API
            → one generated batch allocation per admitted shared build
            → telchar-nomad-worker
            → allocation-side Nix daemon
            → authenticated WebSocket callback to gateway
```

The gateway and worker stores have different roles:

- the **gateway store** is durable authority for admitted inputs, validated outputs, retention, and client result transfer;
- the **worker store** executes builds and may be a warm shared host store or an allocation-local store.

Do not point Telchar at a client process's own Nix store. Recursive store locking can deadlock builds.

## Images

Build the four OCI archives from this repository:

```bash
nix build .#telchar-oci
nix build .#telchar-nix-daemon-oci
nix build .#telchar-nomad-worker-oci
nix build .#telchar-ssh-ingress-oci # optional
```

Load them into your container tooling, publish them to your registry, and replace the jobspec variables with immutable digest references. Mutable tags make deployment and recovery evidence ambiguous.

## Required external resources

Before registering the job, provide:

1. An existing Nomad namespace, default `telchar`.
2. PostgreSQL reachable from the gateway allocation.
3. A Nomad ACL token that can submit, inspect, and purge generated jobs in that namespace.
4. A stable callback URL reachable from worker allocations.
5. Persistent gateway storage for both `/nix/store` and `/nix/var/nix`.
6. Nix execution access on every eligible worker node.
7. Nomad workload identity configured with audience `telchar-transfer`.

The example reads `database_url` and `nomad_token` from a Nomad Variable:

```bash
nomad var put -namespace=telchar nomad/jobs/telchar/gateway \
  database_url='postgresql://telchar:replace-me@postgres.example.invalid:5432/telchar' \
  nomad_token='replace-me'
```

That is portable example plumbing, not a recommendation to keep long-lived secrets in Nomad Variables. Dynamic Vault credentials or another workload-identity-aware broker are preferable. Telchar consumes file paths so secret delivery can change without changing its configuration model.

The PostgreSQL database must initially be empty or contain a Telchar schema compatible with the deployed image. Telchar applies its embedded migrations while holding singleton ownership.

## Gateway Nix storage

The sibling `telchar-nix-daemon` task is the simplest isolated gateway-store topology. The jobspec bind-mounts two persistent host directories:

```text
/srv/telchar/nix/store → /nix/store
/srv/telchar/nix/var   → /nix/var/nix
```

Change those source paths to directories provisioned on every node where the gateway may reschedule. Persist and restore them together. `/nix/var/nix` contains the database that gives `/nix/store` paths meaning.

Alternatives:

- Nomad host volumes;
- CSI volumes that preserve the required Unix ownership and filesystem semantics;
- a node-pinned gateway with operator-managed local storage;
- a host Nix daemon socket, provided its trust and UID policy is explicitly configured for Telchar.

The jobspec uses UID/GID `995:995`, matching the packaged images. If you rebuild images with another identity, change the gateway command's `--frontend-uid`, task users, directory ownership, and SSH ingress identity together.

## Worker Nix store alternatives

The example mounts a host Nix daemon socket into every generated worker allocation:

```text
/nix/var/nix/daemon-socket/socket
```

This provides access to whatever paths are already valid in that store and lets ordinary Nix configuration own substituters, signatures, credentials, and registration outside the callback session. The current worker checks validity once and requests unresolved paths from Telchar; it does not invoke substitution between that check and the request. The socket must exist at the same path on every node selected by the backend constraints, and Docker must permit the bind mount.

Other supported topology:

- add a backend `prestart` task that prepares allocation-local Nix configuration and directories;
- run an allocation-local daemon supplied through operator driver configuration;
- use another operator-managed daemon URI available inside the allocation.

Telchar does not create arbitrary host mounts. Clients cannot choose worker store policy.

## Callback networking

The Telchar callback listener is plaintext WebSocket.

On a trusted private network, the public callback may target the gateway listener directly using the cleartext WebSocket scheme and port `7443`.

For encrypted transport, terminate TLS in an operator-managed proxy or load balancer:

```hcl
callback_public_url = "wss://telchar.example.invalid/callback"
```

The proxy must:

- preserve WebSocket upgrades;
- preserve `Sec-WebSocket-Protocol: telchar-nomad-transfer-v1`;
- disable request retries;
- use an idle timeout greater than Telchar's configured transfer idle timeout;
- forward one callback connection to the singleton gateway.

Workload identity authenticates the allocation even on a private network. TLS and authentication solve different problems.

Nomad often omits the JWT `iss` claim. The example therefore leaves `verify_issuer = false` while still requiring RS256 signature verification, exact audience, time validity, namespace, job, allocation, and task claims. If Nomad servers configure `oidc_issuer` and emitted tokens contain it, set `verify_issuer = true` and keep `issuer` exact.

## Placement and resources

The backend's constraints select eligible execution nodes. The example selects Linux AMD64 nodes because its backend declares `x86_64-linux`:

```toml
[[backends.nomad.constraints]]
attribute = "${attr.cpu.arch}"
operator = "="
value = "amd64"
```

Add constraints for your storage/socket topology. For example, a node metadata flag can assert that the worker Nix daemon is installed. Do not copy a cluster-specific node class into a generic deployment.

The required `[backends.nomad.resources]` table is the default profile. Optional profiles map an advertised Nix system feature to operator-defined CPU, memory, disk, bounded priority, and additive placement constraints. For example, a derivation requiring `overflow-aws` can select a profile constrained to an operator-approved overflow node class. Base backend constraints remain in force.

A profile-selecting feature is a workload requirement, not authenticated authorization. Keep feature advertisement and mappings explicit, and retain hard resource, priority, queue, concurrency, and placement limits. No mapped feature selects the default profile; one selects its profile; multiple mapped profile features fail as ambiguous. Unknown required features remain incompatible with the backend.

Never infer CPU, memory, priority, or placement from derivation or closure byte size. Those byte counts bound transfer and storage, not compilation demand. Nomad priority affects preemption when enabled; it does not order pending jobs. Dependency readiness and Telchar admission remain separate gates.

Profile constraints may select GPU-capable nodes but do not reserve GPU hardware. Nomad device reservations remain future work in the project roadmap.

Generated execution jobs disable task restart and group rescheduling. A failed allocation is a terminal attempt; Telchar never blindly retries `BuildDerivation` or migrates it to another node.

## Optional SSH ingress

Nomad execution does not require SSH ingress. SSH exists for stock Nix clients using `ssh-ng`.

Two reasonable approaches:

### Packaged ingress image

`telchar-ssh-ingress-oci` runs restricted OpenSSH. It reads SSH identity and authentication files mounted by the operator; the image does not contact Vault or any other credential provider.

The simplest mode uses a static host key and `authorized_keys`:

```text
TELCHAR_SSH_HOST_IDENTITY_MODE=key
TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE=authorized-keys
TELCHAR_SSH_HOST_KEY_FILE=/alloc/data/ssh/ssh_host_ed25519_key
TELCHAR_SSH_AUTHORIZED_KEYS_FILE=/alloc/data/ssh/authorized_keys
```

Certificate mode uses a host key, host certificate, and trusted client CA:

```text
TELCHAR_SSH_HOST_IDENTITY_MODE=certificate
TELCHAR_SSH_CLIENT_AUTHENTICATION_MODE=certificate
TELCHAR_SSH_HOST_KEY_FILE=/alloc/data/ssh/ssh_host_ed25519_key
TELCHAR_SSH_HOST_CERTIFICATE_FILE=/alloc/data/ssh/ssh_host_ed25519_key-cert.pub
TELCHAR_SSH_CLIENT_CA_FILE=/alloc/data/ssh/client-ca.pub
```

Vault is one optional way to populate and rotate certificate-mode files. Run Vault integration in a separate sidecar that writes credentials atomically into a shared volume. The ingress watches those files and reloads OpenSSH when they change. A Vault Agent, Nomad template task, Smallstep client, Kubernetes sidecar, systemd credential service, or operator script can provide the same file contract.

The jobspec contains a commented sidecar sketch based on a production deployment. The ingress process starts as root so `sshd` can perform normal privilege separation; authenticated sessions run as packaged UID `995`, which must match the gateway's `--frontend-uid`.

### Operator-managed OpenSSH

Use your own hardened OpenSSH task or external host. Its accepted keys or CA principals must force:

```text
telchar-ssh-forced-command
```

Disable forwarding, PTY, tunnels, user environment, user commands, and agent forwarding. Set `TELCHAR_IPC_SOCKET` to the gateway socket. If ingress is outside the allocation, expose the IPC boundary through a deliberately designed local transport; do not turn the Unix socket into an unauthenticated network service.

SSH host keys, certificates, client CA material, and authorized keys remain operator-owned files. They do not belong in PostgreSQL or Telchar's backend configuration.

## Validate and register

Set variables in a `.vars.hcl` file that is not committed:

```hcl
namespace           = "telchar"
datacenters         = ["dc1"]
gateway_image       = "registry.example.invalid/telchar@sha256:replace-me"
nix_daemon_image    = "registry.example.invalid/telchar-nix-daemon@sha256:replace-me"
worker_image        = "registry.example.invalid/telchar-nomad-worker@sha256:replace-me"
nomad_api_endpoint  = "https://nomad.example.invalid:4646"
callback_public_url = "wss://telchar.example.invalid/callback"
gateway_store_path  = "/srv/telchar/nix/store"
gateway_state_path  = "/srv/telchar/nix/var"
worker_daemon_socket = "/nix/var/nix/daemon-socket/socket"
```

Then use Nomad's parser and planner directly:

```bash
nomad job validate -var-file=deployment.vars.hcl examples/nomad/telchar.nomad.hcl
nomad job plan     -var-file=deployment.vars.hcl examples/nomad/telchar.nomad.hcl
nomad job run      -var-file=deployment.vars.hcl examples/nomad/telchar.nomad.hcl
```

Review the plan. In particular, verify bind mounts, namespace, callback routing, ACL scope, workload-identity audience, placement constraints, and immutable image digests.

## Client smoke test

After adding SSH ingress, direct temporal SSH options are enough to test without installing client-wide configuration:

```bash
export NIX_SSHOPTS='-F /path/to/temporary/ssh_config'

nix --extra-experimental-features nix-command \
  store info --store ssh-ng://telchar@telchar.example.invalid
```

A normal distributed-builder configuration is client policy. Telchar qualification does not require installing persistent SSH credentials on every client daemon.

See [`docs/nomad.md`](../../docs/nomad.md), [`docs/operations.md`](../../docs/operations.md), and [`docs/ipc-and-transfer-messages.md`](../../docs/ipc-and-transfer-messages.md) for protocol, operational, and trust-boundary details.
