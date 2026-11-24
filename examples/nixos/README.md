# Operator-owned NixOS compositions

Telchar exports one service module (`nixosModules.telchar`, also `default`). These
examples are ordinary files, not additional supported module exports. Copy and
adapt the composition you need into your deployment repository.

## Local PostgreSQL and regular OpenSSH

Import the Telchar service module and `local.nix`, supply its package and SSH
identity adapter, then configure real authorized keys:

```nix
{ inputs, pkgs, ... }:
{
  imports = [
    inputs.telchar.nixosModules.default
    ./local.nix
  ];
  _module.args.telcharSshCommand = inputs.telchar.lib.sshForcedCommand;
  services.telchar.package = inputs.telchar.packages.${pkgs.system}.telchar;
  users.users.telchar.openssh.authorizedKeys.keys = [
    # Operator-provided public keys.
  ];
}
```

`local.nix` explicitly provisions PostgreSQL, orders Telchar after database setup,
grants access to the host Nix daemon, and creates retained GC-root storage.
`openssh.nix` uses regular `services.openssh` with a restricted `Match User`
command. It does not create a separate SSH daemon. Other host users retain their
own SSH configuration.

The adapter consumes server-authenticated `ExposeAuthInfo` metadata. A client
command alone cannot supply trustworthy identity. Do not allow clients to set
`TELCHAR_AUTHENTICATED_*` environment variables or bypass the forced command.
Server host certificates, trusted client CAs, authorized principals, and their
rotation are OpenSSH/operator configuration, not Telchar service options.

Clients use `ssh-ng://telchar@build-host` with stock Nix. The host store must not
also serve local client workloads directed back at Telchar: recursive store
locking can deadlock builds. Persist database, store, import spool, and retained
GC roots together according to your recovery requirements.

## Dedicated gateway host

`host.nix` and `standalone.nix` illustrate an explicit dedicated-sshd composition,
optional local PostgreSQL, host-store permissions, certificate installation, and
Nomad-token installation. These helpers are deployment policy; none are imported
by the public Telchar service module. Supply `telcharSshCommand` as above.

## Minimal AWS gateway with static SSH builders

Use two operator-provisioned NixOS EC2 instances in one private network: a
single-active gateway with local PostgreSQL, and a trusted SSH builder. No Vault,
RDS, Nomad, instance discovery, or AWS API credentials are required by this
composition. It does not provision EC2 instances, security groups, or disks.

Gateway module:

```nix
{ inputs, pkgs, ... }:
{
  imports = [ inputs.telchar.nixosModules.default ./aws-ssh.nix ];
  _module.args.telcharSshCommand = inputs.telchar.lib.sshForcedCommand;
  services.telchar.package = inputs.telchar.packages.${pkgs.system}.telchar;
  users.users.telchar.openssh.authorizedKeys.keys = [
    # Build clients' operator-provided public keys.
  ];
}
```

Import `ssh-builder.nix` on the builder; set its authorized key to the gateway's
public key. Replace `builder.internal.example.com` in `aws-ssh.nix` with your
builder's private DNS name. Both instances use `x86_64-linux` in this example.
Keep normal administrator SSH access separate from the restricted build users.

Provision the gateway private key at `/var/lib/telchar/ssh/id_ed25519`, owned by
`telchar`, mode `0600`. Install the builder's independently verified SSH host key
in `/var/lib/telchar/ssh/known_hosts`, owned by `telchar`; do not disable host-key
checking or put private keys in Nix expressions/the Nix store. The builder account
is a trusted Nix-daemon user, not a hostile-tenant boundary.

Allow client SSH to the gateway only from your client networks; allow builder SSH
only from the gateway security group/private network. Neither PostgreSQL nor a
Nomad callback port needs public exposure. Size and persist gateway `/nix` and
`/var/lib` (including PostgreSQL, import spool and GC roots) on EBS; do not replace
or terminate instances as a persistence strategy. Supply ordinary NixOS EC2 boot,
filesystem and `system.stateVersion` configuration separately.

Clients use `--builders 'ssh-ng://telchar@gateway.example.com x86_64-linux'`
with their local Nix daemon. Never direct the gateway's own store workloads back
through itself. This example builds remotely; it does not publish images or
execute generated deployment commands.

## AWS, Vault, external PostgreSQL, and Nomad workers

`aws.nix` composes the dedicated-host example with `vault-aws.nix` and its
`cache_credentials.py` helper. It illustrates the gateway deployment topology:
EC2 instance-profile authentication, a persistent EBS volume and dedicated store,
external PostgreSQL credentials, Vault-issued host certificates and Nomad tokens,
and builds dispatched to an existing Nomad cluster. Telchar itself runs under
systemd, **not inside Nomad**.

Supply the Telchar package, an immutable worker image, and your Nomad endpoint:

```nix
{
  imports = [ inputs.telchar.nixosModules.default ./aws.nix ];
  _module.args.telcharSshCommand = inputs.telchar.lib.sshForcedCommand;
  services.telchar.package = inputs.telchar.packages.${pkgs.system}.telchar;
  services.telchar.deployment = {
    nomadEndpoint = "https://nomad.example.com";
    workerImage = "registry.example.com/telchar-nomad-worker@sha256:<published-digest>";
  };
}
```

Replace example DNS names, Vault paths/roles, cache settings, certificate
principals, capacity, issuer policy, and endpoints before use. Provision your
own boot/root filesystem and the `/var/lib/telchar` filesystem labeled `telchar`.
The volume must match the instance availability zone. Supply these instance tags:

- `TelcharPersistentVolumeId`
- `TelcharDatabaseSecretArn`
- `TelcharRdsCaBundleUrl`
- `TelcharLifecycleHookName`
- `TelcharAutoscalingGroupName`

The instance profile needs scoped EBS, Secrets Manager, and Auto Scaling lifecycle
permissions. Vault AWS auth, SSH signing roles, Nomad credentials, cache secrets,
DNS, and callback TLS termination must already exist. This example does not
provision cloud infrastructure or certificate authorities. The callback URL uses
WSS; Telchar's listener requires operator-owned TLS termination.

Credentials are protected runtime files, never embedded in Nix configuration.
Vault fetches candidates; installation validates and atomically replaces files.
Nomad rotation signals Telchar with SIGHUP. Backend token consumption and reload
are Telchar functionality; credential acquisition and installation are deployment
responsibilities.

The production deployment and provider-specific tests belong to
[telchar-gateway](https://github.com/Techcyte/telchar-gateway). These examples do
not make Vault/AWS tests part of Telchar's product suite.
