# Lambda MicroVM backend

**MicroVM integration is experimental, not production-ready.** The gateway
launches one ARM64 execution per admitted build, validates outputs in its own
store, and retains durable cleanup obligations. Output-only recovery never adopts
or relaunches interrupted MicroVM executions. AWS Lambda MicroVMs are distinct
from ordinary Lambda functions.

## Read-only obligation inspection

`telchar operator microvm [--limit N]` reads persisted capacity and known-instance
cleanup obligations without contacting AWS or acquiring daemon ownership. It
requires an existing migrated database. The default limit is 64; accepted range
is 1–256. Rows are ordered by creation time and intent ID, oldest first; this is
a bounded snapshot, not a complete inventory when more rows exist.

Each row exposes intent/backend/account/region/provider identity, phase,
`capacity_reserved` and `cleanup_pending`. A risk-released known instance remains
visible until termination is observed. Unknown-identity risk-released intents
are not included in this obligation view. Client tokens, credential verifiers,
request/configuration digests and operator-supplied evidence are excluded.
This command does not mutate state or provide administrative authority. Row and
field bounds do not establish a total database/network deadline.

`telchar operator microvm-audit [--limit N]` reads immutable resolution summaries
with the same default and maximum row limits. Rows use descending resolution ID
(sequence allocation order, not a guarantee of transaction commit order). Each
summary includes resolution/intent/provider identity, action, kernel-authenticated
actor UID, owner generation and database recording time as Unix seconds. Records
remain visible after cleanup finishes, including risk releases without a provider
identity. Free-form reasons and evidence references are deliberately omitted: they
may contain sensitive operator text. This is a bounded audit summary, not a full
forensic export or a total database/network deadline guarantee.

## Execution and qualification boundaries

The [backend ADR](adr/microvm-backend.md) describes durable launch identity,
capacity quarantine, authenticated admission and output-only recovery.

- Launch retries are disabled. Ambiguous launches retain capacity until explicit
  operator reconciliation; elapsed time alone does not establish remote absence.
- Ordinary cleanup releases capacity only after exact observed `TERMINATED`.
  Audited risk release is separate from known-instance cleanup.
- Workers connect outward over WSS to `/microvm`; TLS termination is operator-owned.
  Nomad authentication remains separate. Gateway registration of every expected
  output is required before success.
- Strict `[[backends.microvm]]` policies share bounded admission and cleanup per
  account/region. Policy changes require restart. Retained obligations require
  configured account/region authority, including across backend aliases.
- Startup verifies the account and exact image/version. Changed operation
  credentials require re-attestation before use.
- The runtime installs a protected execution binding and starts a fresh daemon
  and worker inside an execution PID namespace. Missing namespace capability
  rejects execution before commands start.

Local integration fixtures use real PostgreSQL, TLS and isolated Nix stores but
simulate provider responses. Default transport fixtures disable sandboxing;
the [native packaged-runtime contract](../tests/microvm/runtime-exchange.md)
separately exercises production sandbox policy. Neither establishes current
live AWS qualification or production readiness.

Live failure, restart, fencing and concurrency qualification, representative
resource sizing, deliberate disk exhaustion, provider expiry and billing remain
unqualified. Successful builds or configuration checks do not close those gaps.
Residual snapshot/PRNG risk is accepted, without a claim of cryptographic
uniqueness; preserve fresh daemon/worker startup and production sandbox policy.

## Unattended AWS credentials

The gateway uses the AWS Rust SDK credential chain, bounded by
`provider_timeout_seconds`. Environment credentials and named profiles containing
`aws_access_key_id`, `aws_secret_access_key` and optional `aws_session_token` are
supported. A protected shared-credentials profile is suitable when the deployment
already supplies and rotates those credentials. Telchar does not acquire them.
The compiled SDK does not enable SSO, interactive login or `credential_process`;
a CLI profile that depends on those features is not a daemon credential source.

For a NixOS service, load the file through the existing systemd credential input:

```nix
services.telchar = {
  credentials = [{
    name = "aws-credentials";
    source = "/run/secrets/telchar-aws-credentials";
  }];
  environment = {
    AWS_SHARED_CREDENTIALS_FILE = "%d/aws-credentials";
    AWS_CONFIG_FILE = "/dev/null";
    AWS_PROFILE = "gateway";
    AWS_EC2_METADATA_DISABLED = "true";
  };
};
```

`%d` expands to the service's systemd credential directory. The protected source
file must be readable by the credential loader, outside the Nix store, and contain
an operator-selected profile:

```ini
[gateway]
aws_access_key_id = OPERATOR_SUPPLIED_ACCESS_KEY
aws_secret_access_key = OPERATOR_SUPPLIED_SECRET_KEY
aws_session_token = OPERATOR_SUPPLIED_SESSION_TOKEN
```

Never put these values in Nix expressions, generated TOML, service environment
values, images or logs. Omit the session-token line only for credentials that do
not require it. Remove inherited `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
`AWS_SESSION_TOKEN`: environment credentials take precedence over the file.
Metadata lookup is disabled in this profile deployment to prevent an unintended
instance-role fallback. This is a supported input shape, not selection of a
production principal or a recommendation to create long-lived keys.

Systemd copies credentials at service start. Refresh the protected source and
perform a planned drain/restart before expiry; file replacement and SIGHUP do not
promise live SDK or systemd credential rotation. Keep account/region cleanup
authority available until every retained obligation is resolved. Startup requires
signed STS account attestation and exact image verification; operation credentials
are re-attested when their resolved identity changes. Missing, expired or
wrong-account credentials fail closed and must not trigger automatic mutation
retries.

## Same-source ARM worker and image context

The flake exports `packages.aarch64-linux.telchar-nomad-worker`, including
`bin/telchar-microvm-worker`. It uses the same filtered source and coordinated
version as `packages.x86_64-linux.telchar`. Build on an ARM64 Linux builder:

```sh
nix build --no-link .#packages.aarch64-linux.telchar-nomad-worker
```

The ARM64 OCI execution image uses the coordinated release version and is
published as `ghcr.io/techcyte/telchar-microvm-execution`. Build its archive with:

```sh
nix build --no-link .#packages.aarch64-linux.telchar-microvm-execution-oci
```

`lib.microvmAwsContext` constructs the AWS build context from that worker and the
pinned Nix closure. From a Git checkout of the selected revision:

```sh
nix build --impure --no-link --print-out-paths --expr '
  let flake = builtins.getFlake "git+file:///absolute/path/to/telchar-checkout";
  in flake.lib.microvmAwsContext'
```

Both compositions require deployment to mount a public DER-encoded gateway TLS
trust certificate read-only at `/run/telchar-microvm/trust.der`. No certificate
is an image-build input or image content. Never mount a private key. Operators
own certificate identity, validity, TLS termination and rotation. Certificate
rotation replaces the runtime mount and starts a fresh execution; it does not
require rebuilding the software image. There is no insecure or fixture-trust
fallback. Missing, empty or oversized trust prevents readiness and run admission;
the worker validates DER parsing, hostname and TLS certificate trust before
connecting. `/run/telchar-microvm` is root-only and also holds the per-execution
binding; mount the certificate file, not a directory that hides that binding.

The AWS context contains its Dockerfile and worker/Nix runtime closure, not a
callback server or private key. The OCI image is a dedicated Linux execution
runtime, not the AWS base image or an AWS snapshot. Deployment owns runtime
certificate provisioning and AWS image construction. Use exact image versions
and retain preceding artifacts for rollback.

Derivation evaluation establishes source/version correspondence, not successful
ARM compilation or runtime qualification. Image construction, selected-artifact
sandbox qualification and AWS lifecycle evidence remain separate release gates.
The flake does not create an AWS image, connector or deployment.

## Image version policy

`image_version` selects an exact numeric image version, such as `"1"` or
`"1.0"`. The major component must be positive; an optional minor component can
be zero. Components use canonical decimal digits without leading zeroes, with
at most 128 bytes overall. Aliases such as `"latest"` are rejected. Telchar sends
the configured string unchanged and requires AWS image verification to return
that exact version in `SUCCESSFUL` and `ACTIVE` state.

## VPC egress policy

Every `[[backends.microvm]]` policy requires an operator-owned
`vpc_egress_connector_arn` in the configured AWS account and region:

```toml
vpc_egress_connector_arn = "arn:aws:lambda:us-west-2:116392764028:network-connector:builder"
```

Telchar explicitly sends this connector in `RunMicrovm.egressNetworkConnectors`.
Omitting it, using an empty value, or selecting another account or region rejects
configuration; there is no implicit public-internet egress mode. The connector
participates in both the deployment fingerprint and the durable launch request
digest. Changing the ARN requires a restart and cannot alter a reserved request.
Use a version-qualified ARN such as `network-connector:builder:12` to pin VPC
configuration. An unqualified ARN follows AWS connector updates; its identity
digest does not freeze the underlying routes or security groups.

Operators provision the VPC egress connector, its IAM permissions, subnets,
security groups and routes. Telchar does not create or modify them. VPC egress
routes outbound traffic through that network; internet access can still exist
through operator-configured NAT/routes. It does not expose SSH or replace the
AWS-managed authenticated HTTPS ingress endpoint. The callback address must be
reachable through the selected connector, and WSS certificate validation remains
mandatory. Configuration validation checks ARN identity and syntax, not connector
readiness, routing or security-group policy; AWS launch rejection remains a
fail-closed, non-retried provider failure.

## Local administrative resolution

An optional `TELCHAR_ADMIN_SOCKET` path enables a dedicated daemon-owned Unix
socket in a private runtime directory. Socket mode is `0600`; the server also
requires kernel peer UID 0 before decoding any request. Frontend envelopes and
caller-supplied actor fields confer no authority. The daemon serves one bounded
request at a time, separately from build admission. Frames are limited to 4096
bytes with a five-second absolute receive deadline.

NixOS deployments can enable it explicitly without changing the daemon user:

```nix
services.telchar.adminSocketPath = "/run/telchar/admin.sock";
```

The option defaults to `null` (disabled), requires a separate socket directly
below the service-owned `/run/telchar` directory, and sets `TELCHAR_ADMIN_SOCKET`.
Do not also set that environment variable through `services.telchar.environment`.
The runtime directory remains `0700` and socket remains `0600`; daemon ownership
of these paths does not grant its nonroot UID administrative authority. No TCP
listener, firewall rule or infrastructure is provisioned by this option.

```sh
telchar admin microvm risk-release \
  --socket /run/telchar/admin.sock --intent INTENT \
  --provider-id EXACT_PROVIDER_ID \
  --reason 'Incident decision: remote execution risk accepted' \
  --evidence 'incident:123' --accept-remote-execution-risk
```

Omit `--provider-id` only when the persisted provider identity is absent. Exact
identity, current daemon ownership and unresolved state are checked transactionally.
A successful release appends an immutable audit record; it does not assert remote
termination or erase known-instance cleanup. `released: false` means no matching
eligible intent changed.

Administrative transactions use a 250 ms lock timeout and a one-second statement
timeout. Each database checkout also has a five-second absolute client-side budget
covering pool wait, validation, queries and transaction rollback. Expired or failed
connections are discarded; application mutations are not automatically retried.
Administration shares one five-second absolute request budget from frame decoding
through identity lookup, ownership verification, health-lock acquisition, provider
verification, final mutation and response writing. Scoped database handles retain
the existing pools. Daemon shutdown signals the active request before joining its
handler; database and provider waits observe cancellation, while socket closure
interrupts frame I/O. The CLI response timeout alone does not signal cancellation.

Ownership release on shutdown is best effort, with a separate 250 ms client-side
budget for the exact owner token and generation. A stopped server can retain its
transaction locks after client transport closure, so the lease may remain until
expiry. Bounded client teardown is not proof of immediate server rollback or
successful lease removal. The normal daemon loop fails on administrative listener
failure; the `--once` path does not poll that health check. This interface remains
experimental.

A timeout, lost response or generic rejection is not proof that a mutation rolled
back. COMMIT can succeed durably before its response is lost. The CLI reports an
unknown mutation outcome for request-write failures, response failures and generic
server rejection. This is conservative: a rejected request may never have reached
mutation, but the response does not distinguish that from a lost commit result.
Argument validation and connection failures remain pre-request errors. Inspect the exact
intent with `telchar operator microvm` and its immutable resolution summary with
`telchar operator microvm-audit` before deciding whether another administrative
action is appropriate. Both reports are bounded views: absence from a limited
result is not proof that no mutation occurred. Do not automatically retry an
uncertain mutation.

Risk-released intents with no provider ID still need evidence-backed identity
reconciliation. The provider lookup exposes an instance's ID and image/version,
not the persisted launch token or request digest; matching image/version/time is
not proof that the instance belongs to an intent. Administrative attachment must
verify provider correspondence and audit the launch-linking evidence, remain
cleanup-only, and preserve any prior capacity release. The ordinary launch-binding
API is not an administrative repair interface.

```sh
telchar admin microvm attach-identity \
  --socket /run/telchar/admin.sock --intent INTENT \
  --provider-id EXACT_PROVIDER_ID \
  --reason 'Recovered exact launch response' \
  --evidence 'incident:123/launch-response'
```

Attachment reads the persisted account/region/image/version, selects configured
account/region authority, and checks exact provider metadata outside database
transactions. Credential resolution, re-attestation and metadata lookup share a
provider budget capped at two seconds and the remaining absolute request budget.
Timeout during provider verification prevents the attachment mutation from starting.
Database reads, pool validation, pre/post-provider ownership checks and final
mutation use the same request deadline and cancellation flag through shared-pool
handles; they do not receive a fresh five-second request budget at each stage.
It rechecks service ownership afterward and atomically closes admission, attaches
the ID and records the audit under the database fence.
`attached: true` reports that transition; it neither releases capacity nor permits
worker admission, adoption or relaunch. The evidence reference is an operator
assertion of launch linkage, not automatically fetched or independently verified.
The first configured authority for the account/region is used; no credential
failover or metadata-based instance discovery occurs.

```sh
telchar admin microvm verify-termination \
  --socket /run/telchar/admin.sock --intent INTENT \
  --provider-id EXACT_PROVIDER_ID \
  --reason 'Confirm provider terminal state' --evidence 'incident:123'
```

Termination verification reads the exact persisted intent/provider account and
region, selects matching configured authority, and requires an attested exact-ID
`TERMINATED` response. It shares the two-second provider budget and post-lookup
ownership check with attachment. Observation, capacity release and immutable audit
commit together under the database fence; any previous risk-release timestamp is
preserved. `terminated: true` reports this administrative transition, not a DELETE
acknowledgment. Missing, mismatched or already observed identities reject the
request. This command does not send a termination request or relaunch work.
If ordinary cleanup completes during provider lookup, the administrative
compare-and-set returns `terminated: false` without fabricating an audit.
Administrative lost-COMMIT-response integration, health-lock fault coverage and
live AWS correspondence remain unqualified.

## Local validation

Before running compilation or qualification, establish a hard resource limit
covering the command and its descendants. Serial test threads and
`CARGO_BUILD_JOBS=1` reduce concurrency but do not enforce memory limits.

Within that boundary and the flake development shell, implemented local
boundaries can be checked with:

```sh
bash tests/microvm/verify-implementation.sh
```

This runs formatting, workspace all-target compilation, focused PostgreSQL/HTTP/
TLS/worker tests and Python runtime tests including a private Nix daemon. The
private Nix supervision and dedicated-worker fixtures disable sandboxing; they do
not qualify production containment or ARM image execution. Daemon routing tests
use authenticated local IPC rather than stock OpenSSH ingress. The checkpoint does
not run full workspace tests, Clippy, stock-client VM contracts or live AWS
qualification. Local checks use isolated state and require nonroot Linux process
and socket permissions, user namespaces, Python with `os.unshare`, and util-linux
`unshare`/`setpriv`. Preserve the pinned flake dependencies and test TLS
fixtures. No AWS credentials or resources are needed.

## Runtime packaging and snapshot limits

The AWS build context uses `deploy/microvm/Dockerfile` and the pinned
`public.ecr.aws/lambda/microvms:al2023-minimal` base. Amazon-packaged Python runs
the lifecycle listener; the Docker build checks namespace API availability.
Dedicated build accounts are added without replacing AWS system accounts.
The unmodified Nix/worker closure and mandatory sandbox policy are included;
Nix daemon and worker start only after an accepted run hook. No custom OpenSSL
patch is used. Context construction is not full OCI or AWS runtime qualification.

`nix/microvm-execution.nix` provides the ARM64-only dedicated execution OCI image
with a runtime-mounted public DER trust certificate and the coordinated worker
version. Registry publication is not AWS runtime or snapshot qualification. The
binding lives under root-only `/run/telchar-microvm`. Lifecycle `ready`,
`validate`, `suspend` and `resume` accept only idle state; completed or failed execution cannot become
a reusable snapshot through those hooks. That guard does not erase secrets from
memory or establish provider snapshot behavior.

`nix/microvm-worker.nix` instead packages the example worker with the probe
composition in `nix/microvm-probe.nix`. Its daemon starts before snapshot; it is
not the dedicated fresh-process runtime. Probe images and example exchanges are
not substitutes for the configured gateway and packaged worker.

AWS's [snapshot guidance](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
states that snapshots include process memory, disk, connections and file
descriptors, and requires AWS-patched OpenSSL for applications using OpenSSL.
The AWS base does not replace Nix-store libraries. The Nix closure does not
establish compliance or snapshot cryptographic uniqueness. Residual snapshot/PRNG
risk is accepted without custom OpenSSL packaging or external sign-off; keep
fresh daemon/worker startup and never snapshot active builds.

## Sources

- [Images, capabilities, sizing](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images.html)
- [Lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/microvms-launching.html)
- [Networking](https://docs.aws.amazon.com/lambda/latest/dg/microvms-networking.html)
- [Snapshots](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
- [Run API and idempotency](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_RunMicrovm.html)
- [Pricing](https://aws.amazon.com/lambda/pricing/)
