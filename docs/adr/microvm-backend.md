# Managed MicroVM build execution

## Status

Proposed; incomplete design under review. Not implemented. Travis approved the
first-release launch-ambiguity and output-only recovery limits below; that
approval does not accept the whole ADR or authorize implementation.

## Scope

Add an ARM64 build backend executing one admitted build per AWS Lambda MicroVM.
Preserve stock Nix ingress, subject-fair coalescing, gateway-store authority and
single-active database ownership. This proposes a narrow exception to the
[architecture](../design.md) non-goal of provisioning compute: launching and
terminating one managed execution is backend work. Publishing images, creating
networks/IAM roles, fleet management and deployment remain operator-owned.
Travis must approve this boundary before implementation.

The [experiment](../microvm.md) proved substantial native compilation and usable
validated outputs, not production recovery, capacity or snapshot cryptographic
safety. Accepted experimental gaps become implementation qualification gates.

## Existing extension points

Paths are repository-root-relative:

| Boundary | Owner | Design constraint |
| --- | --- | --- |
| Target compatibility/capacity | `crates/telchar/src/backend/mod.rs` | Exact system/features; existing permits, no separate scheduler |
| Selection, execution, adoption | `crates/telchar/src/backend/routing.rs` | Explicit backend branch; no plugin registry |
| Shared build/attempt identity | `crates/telchar/src/persistence/shared_builds.rs` | One active admitted execution; preserve attempt agreement |
| Startup reconciliation | `crates/telchar/src/shared_build/recovery.rs` | Exact valid gateway outputs win first |
| Ownership | `crates/telchar/src/service/singleton_ownership.rs` | Database generation fences durable mutations, not AWS itself |
| Worker build/transfer | `crates/telchar-nomad-worker/src/lib.rs` | Reuse authenticated session mechanics without Nomad identity |
| Callback admission | `crates/telchar/src/nomad/callback.rs` | Nomad allocation/job checks cannot authenticate MicroVMs |

Existing `BackendKind` has local, static SSH and Nomad variants. Recovery
capabilities distinguish output-only from adoptable execution. A MicroVM launch
intent must have its own durable identity before the provider assigns an ID;
it must not pretend that the local executor aggregate is a remote launch ledger.
Current Nomad execution optionally retries infrastructure failures through its
configured `max_retries`; MicroVM policy must be specified independently rather
than inheriting that branch. No existing retry behavior is changed by this ADR.

## Provider contracts verified so far

Checked public documentation during design; no account API calls:

- [RunMicrovm](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_RunMicrovm.html)
  accepts a case-sensitive `clientToken` of 1–128 characters for idempotency.
  The referenced contract does not specify token retention or recovery after
  that window. Do not treat arbitrary future replay as guaranteed safe.
- [ListMicrovms](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_ListMicrovms.html)
  filters by image/version; returned items contain ID/image/version/start/state,
  not the client token or hook payload. Image/time proximity is not identity.
- [GetMicrovm](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_GetMicrovm.html)
  requires an exact ID and explicitly describes state as eventually consistent.
  A status poll is not worker readiness or proof that no execution exists.
- [Running and using MicroVMs](https://docs.aws.amazon.com/lambda/latest/dg/microvms-launching.html)
  documents a maximum 16 KB runtime payload and a provider-injected `microvmId`
  in the `/run` body. The runtime payload is launch-specific. Endpoint traffic
  begins after the hook returns200; an outbound worker connection need not wait
  for gateway receipt of the launch response, as the experiment demonstrated.
- The same guide says asynchronous applications should disable automatic
  suspension or configure an appropriate idle duration. CPU activity alone
  must not be treated as endpoint traffic. Production build policy should not
  suspend active execution or use snapshots as a build-recovery mechanism.
- [TerminateMicrovm](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_TerminateMicrovm.html)
  targets an exact ID. A termination request is not independent observation of
  completed termination; bound cleanup polling and preserve unresolved cleanup.

## Decision summary

Launch ambiguity and restart behavior have narrow first-release approval below.
Remaining sections propose exact admission, bounded cleanup and fresh-process
image startup. Unknown provider token-retention and acceptance-delay guarantees
remain explicitly unknown; this design does not require them for launch replay
or automatic unknown-ID capacity release. Snapshot security and deployment IAM
review are implementation release gates, not claims established by this ADR.

## Proposed launch transaction

Use a gateway-generated random execution-intent ID as the durable backend
execution identity. Store provider ID separately; neither derivation identity
nor provider list order is a substitute. Create one launch record linked to the
exact shared-build attempt, under current ownership and before any remote call.

Proposed launch record fields:

- intent ID, attempt ID, admitted specification digest and backend name;
- immutable account/region/image ARN/version and deployment configuration digest;
- client token and digest of the exact launch request, excluding secret material;
- launch phase, creator ownership generation and creation timestamps;
- admission cutoff, provider maximum duration, nullable provider ID and cleanup
  obligation/status; all state transitions compare the exact attempt/intent;
- bootstrap credential verifier, expiration and session-consumed state, never
  bearer bytes, NARs or logs. The credential policy below intentionally prevents
  bearer reconstruction after restart.

The request digest must not be presented as replayable request content. A digest
cannot recover omitted secret payload bytes. The first-release proposal does not
replay `RunMicrovm` after an ambiguous outcome; explicitly disable SDK automatic
launch retries too. Pass a unique client token as defense in depth, not as an
undocumented restart-recovery guarantee. Reads and exact-ID termination may use
bounded backoff without creating replacement execution.

In one fenced transaction, mark the intent `dispatching` and grant exactly one
local dispatch. Commit before calling AWS. A crash between that commit and the
call is deliberately treated as possibly launched; it cannot be distinguished
from a lost response. This trades availability for avoiding duplicate builds.
If a response arrives, validate account-scoped client context, expected image and
version, and bounded response identity before atomically binding provider ID.
Conflicting IDs are an incident, never a last-write-wins update.

`prepared`, `dispatching`, `bound`, `ambiguous` and `closed` describe the launch
record, not additional shared-build states. Cleanup status is independent of
shared-build success/failure so a successful registered result cannot erase an
unconfirmed termination obligation. Append-only migration and ownership trigger
coverage are required for every mutable table.

### Crash and ambiguity matrix

| Boundary | Proposed handling |
| --- | --- |
| Before launch record commit | No remote call permitted; ordinary queued/admission handling |
| Prepared but not dispatching | Original issuing process may dispatch after cutoff/cancellation checks; restart abandons unused intent because bearer is not persisted |
| Dispatching committed, no bound ID | Treat as ambiguous; never issue another launch |
| Response received, ID commit fails | No build authorization; preserve exact response in surviving process memory for fenced reconciliation by current owner, otherwise ambiguity remains |
| Exact ID durably bound, worker not admitted | Current owner may authorize the original bounded bootstrap session; no new launch |
| Worker session admitted, gateway dies | Outputs-first reconciliation; incomplete transfer fails first-release recovery; retain cleanup obligation |
| Some outputs registered | Not success until all expected outputs validate; no output-set inference from provider termination |
| All outputs valid, terminal result absent | Existing outputs-first recovery records success; still reconcile cleanup |
| Termination requested, confirmation missing | Bounded retry/query exact ID; retained cleanup-pending status |
| Provider ID unknown through cutoff | Fail request with launch-ambiguous classification, retain capacity/cleanup quarantine; no inferred absence from listing |

Unknown-ID quarantine cannot be safely released solely at `intent creation +
maximum duration`: the provider may accept/start later, and the documented API
request timeout does not establish cancellation of server work. A defensible
release bound needs a provider-guaranteed acceptance/start limit or an explicit
operator reconciliation decision. This is a release blocker for unattended
capacity reuse, not a reason to silently add a guessed time margin. Known IDs
have exact cleanup polling; provider maximum duration remains defense in depth.

## Proposed early worker rendezvous

Replace the experimental opaque TCP relay with an application-level bounded
bootstrap rendezvous. The worker receives intent ID, expiring random capability,
callback URL and trust configuration only through `/run`, and freshly executes
the TLS client after restore. The operator owns public TLS termination; Telchar
must not acquire native TLS termination as a side effect of this backend.

Authenticate a small bootstrap message before retaining any wait slot. Capability
possession proves knowledge of launch-specific initialization, not independently
attested AWS identity. Until the API-returned exact ID is durably bound and
matches the hook-reported ID, send no manifest, inputs or build authorization.
Do not bind the provider ID solely from a bearer client's assertion. Bounded
waiting ends at admission cutoff; malformed/unknown/foreign sessions fail closed.
Global/per-backend pending-connection limits and per-message byte/time limits
prevent a capability-free connection flood from consuming execution capacity.

After binding, one fenced admission transaction verifies current owner, exact
attempt, nonterminal shared build, matching provider ID, credential verifier,
expiry and unused session slot, then consumes the slot. A crash after consumption
but before manifest delivery fails closed; no transparent session replay. Hook
idempotence is distinct from reconnect permission. Repeated identical `/run`
cannot start another worker; different binding must be rejected.

This conservative proposal does not use a callback to rescue a lost launch
response. That alternative would need a stronger documented identity binding or
explicitly accepted trusted-image capability attestation model. Record it as an
alternative rather than quietly broadening credential authority.

## Ownership and first-release recovery boundaries

PostgreSQL lease verification immediately precedes remote mutations but cannot
make AWS calls atomic with ownership. A paused former owner may resume between
verification and remote launch. It must never authorize a build after losing its
fenced database authority; new ownership never dispatches an intent already
marked dispatching. This limits duplicate launches for one intent but does not
prove instantaneous cessation of remote side effects.

Stale owners must stop callback/store activity and must not terminate an execution
based on stale cancellation state. Exact cleanup belongs to current
ownership; queued cleanup survives process death. Database fencing also does not
roll back an already issued Nix store import. Store validation and eventual
outputs-first recovery remain necessary, with no claim of distributed atomicity.

Travis approved output-only build recovery for the first release. A durable
worker reconnect protocol is outside that release. A known provider ID supports
cleanup/inspection, not automatic adoption of a dead WebSocket. After restart,
complete exact gateway outputs win; otherwise fail the interrupted build and
terminate its known execution. Do not label provider RUNNING status as resumable
build recovery. This deliberate availability limitation is approved for the
first release.

## Approved first-release limits

Travis approved these recommendations during design:

- An ambiguous launch fails the build without automatic relaunch or binding
  solely from callback-reported identity. Retain capacity quarantine and cleanup
  obligation until operator reconciliation; do not invent a time-based release.
- After gateway restart, exact validated gateway outputs win. Otherwise fail the
  interrupted build and terminate the known MicroVM; no running-build adoption
  or resumable WebSocket claim.

These decisions constrain the remaining design; approval is not permission to
discard an unknown execution or terminate unrelated resources.

## Capacity quarantine and operator reconciliation

A normal backend permit governs active dispatch, but an in-memory permit cannot
represent unresolved remote cost after process death. Add a durable per-backend
execution reservation linked to the intent. Reserve transactionally before
marking dispatching, counting all nonreleased reservations across generations.
The existing fair scheduler still selects work; this reservation is an external
execution safety gate, not another queue or a second quota charged to followers.

Known-ID capacity releases only after observed terminal execution or an explicit
operator resolution. Build completion may release subject admission while remote
capacity remains occupied for cleanup. An ambiguous intent keeps its reservation
after the build fails. Restart reconstructs these obligations before accepting
new MicroVM dispatches. If quarantined reservations consume all configured
capacity, that backend becomes unschedulable; do not borrow capacity from another
backend to resume the same execution. Separate configured aggregate launch-rate
and unresolved-intent limits prevent runaway API traffic.

Keep `telchar operator` read-only. It reports intent/attempt/backend, phase,
creation time, provider ID if known, cleanup status and bounded failure reason;
no capability, hook body or credential hash is exposed. Resolution is a proposed
separate explicit administrative action routed to the active daemon, with local
administrator authentication, current ownership and compare-and-set against the
exact unresolved intent. Do not create an unfenced direct database repair path.

Resolution choices:

1. Attach an exact provider ID only with an audited evidence reference linking
   it to this launch (for example a retained exact successful response recovered
   by the operator). Verify configured account/region and image/version with the
   provider. Image/version/time matching alone is insufficient. Attachment enables
   exact cleanup only; it never resurrects a failed build or authorizes worker
   admission after cutoff.
2. Record verified termination for an already bound ID after an exact query.
3. Explicitly acknowledge unresolved remote-execution risk and release the
   reservation. Require intent ID, reason and evidence reference, record actor,
   ownership generation and database timestamp, and clearly label this as risk
   acceptance, not verified absence. No automatic TTL or blanket reset operation.

Release does not delete the obligation or its audit history. A later exact ID
observation remains visible and eligible for cleanup by current ownership, even
if capacity was manually released. Do not claim token/list matching can discover
all orphan instances. Operators may elect to disable backend dispatch rather
than accept this risk. Administrative command shape and deployment authentication
must be reviewed before implementation; no public mutation endpoint is implied.

## Credential and transport proposal

Generate a fresh 256-bit random bootstrap capability in the gateway after intent
creation, using OS entropy. Persist only its cryptographic verifier, expiry and
consumption status. Carry the bearer only in the bounded `/run` payload and
worker runtime-only0600 file; never in generated Nix configuration, database,
CLI arguments, telemetry or diagnostic bodies. No need to recover the bearer on
restart: approved output-only recovery closes interrupted work. If the process
dies before dispatch, abandon that unused intent rather than silently generate a
different request under its token. Marking dispatching requires a verifier already
committed and the original payload available in the issuing process memory.

Use a dedicated versioned MicroVM bootstrap schema and callback route; the
experimental headers and certificate transport are not a production protocol.
Require `wss://` externally, normal hostname/certificate validation against
operator-configured trust roots, no insecure certificate switch or URL userinfo.
A private local plaintext hop behind operator TLS termination is permitted only
as protected deployment wiring, matching the current product boundary. Reverse
proxy logs must redact authentication, and proxy idle/frame limits must permit
bounded silent compilation without removing application deadlines.

Separate bootstrap verification from authenticated transfer. Prefer the smallest
extraction of backend-independent transfer/session mechanics into a Telchar domain
module, retaining Nomad authentication, allocation verification and wire contract
unchanged. Do not rename the Nomad protocol or broaden its Authentication fields
with fabricated job/allocation IDs. The actual extraction boundary requires a
complete read of callback_service/protocol and their real-store tests before a
concrete module/API is selected. No generic plugin framework or blanket workspace
crate rename is proposed.

MicroVM transfer limits must be operator-configured and mutually validated with
existing gateway admission limits: metadata/path count, per-NAR and aggregate
bytes, chunk size, bounded live logs, staging disk reserve, input/build/collection
and whole-execution deadlines. Stream/stage using production NAR validation and
typed store import. Independent declared metadata, exact admitted outputs and
fixed-output content addresses remain authoritative. Receipt follows successful
gateway registration, never just worker build success. No replay of old logs or
partial transfer resume in the first release.

## Image startup and snapshot safety proposal

Prefer a minimal hook supervisor at snapshot time, with no live Nix daemon,
worker TLS client, gateway connection, per-execution credentials or initialized
OpenSSL-dependent application process. Bake immutable Nix/worker closures and GC
roots on disk; do not snapshot a service solely to avoid startup latency. `/ready`
checks the static artifact/startup prerequisites without starting a build. Image
validation may start fresh processes on its separate validation instance, not
leave them alive in the captured source snapshot.

On `/run`, atomically install the exact binding, start a fresh root Nix daemon
with dedicated nonroot build users, require sandbox=true and
sandbox-fallback=false, then fresh-exec the worker. Startup failure never admits a
build. Nix daemon socket readiness is a protocol check, not a sleep. Duplicate
identical hooks cannot launch another daemon/worker; conflicting hooks fail.
The daemon and worker process group is terminated on local deadline. Reject
active suspend/snapshot requests; do not implement warm suspended builders.

This proposal removes retained application OpenSSL state from Nix, but it is not
proof that every supervisor/library/provider component is snapshot-safe. Inventory
all captured processes and loaded crypto libraries. The [AWS snapshot guidance](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
requires AWS-patched OpenSSL when used in a snapshotted application. A fresh Nix
process linked to Nixpkgs OpenSSL must be explicitly distinguished from retained
OpenSSL state; provider guidance or security review must validate that boundary.
Use the AWS snapshot-safe library for any OpenSSL consumer that must remain alive
at capture, or do not release that image. Merely selecting an AL2023 base does not
replace Nix store-linked OpenSSL. Distinct random samples are not certification.

Pin image ARN and exact numeric version with reproducible package provenance;
never follow a moving version implicitly. Only a successful active version is
schedulable. Monitor the [managed base image lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images.html)
(available/deprecated/expiring/expired/recalled) and require operator rebuilds;
image publication and privileged capability approval remain deployment-owned.
ARM64 only initially; do not advertise x86 through implicit emulation. All OS
capabilities needed for Nix sandboxing must be explicit and qualified.

Set provider maximum lifetime explicitly and local deadlines strictly inside it,
with time reserved for transfer/cleanup; the experiment's600s ceiling is evidence,
not authorization for longer production testing. For outbound-only execution,
set the documented idle threshold beyond maximum execution lifetime, explicitly
set the required suspended-duration field and disable auto-resume, rather than assume autoResumeEnabled=false disables suspension.
[IdlePolicy](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_IdlePolicy.html)
measures inbound proxy traffic and requires a threshold of at least60s; omission
and default behavior must be verified before allowing them. Do not use an idle
policy that can suspend a legitimate silent build.

## Cancellation, deadlines and cleanup lifecycle

Maintain three distinct outcomes: caller attachment, shared build and remote
execution cleanup. Default detach-and-finish only removes the caller attachment;
other followers and admitted execution continue. Operator-configured
cancel-running uses the existing shared-build cancellation decision, not an
individual follower disconnect as authority to kill shared work.

Before dispatch, cancellation atomically closes the unused intent and releases
its reservation. After dispatch, close admission first and fail/cancel the shared
build through existing terminal-state arbitration. A known ID becomes cleanup
pending; an unknown ID becomes ambiguous/quarantined. Do not call Run again in
either case. A response arriving after cutoff may bind the exact ID for cleanup,
never reopen admission or revive the failed build. A late callback cannot renew
the deadline. Concurrent cancellation/success resolves through fenced conditional
terminal mutation; neither handler overwrites an already recorded result.

Separate deadlines cover provider request, hook startup, preauthorization
rendezvous, input setup/import, build, output collection and whole execution.
Use monotonic time within one process and durable database-time cutoffs for
restart/admission decisions. Provider lifetime is a final independent ceiling,
not proof the local worker stops immediately when its socket disconnects. Local
supervisor must kill the whole execution process group, including Nix daemon
children, rather than only the TLS client. Shutdown closes new admission and
boundedly drains sessions; anything incomplete then follows output-only recovery.

Loss of the admitted stream is terminal for that execution session: stop further
transfer, fail incomplete build, queue exact termination. No reconnect, reimport
resume, replacement launch or automatic attempt rotation. Preauthorization
connection retry is distinct: a worker may retry transport establishment within
its original cutoff only while no admission slot has been consumed. A pending
connection owns no session-consumed slot. Once consumed, even loss before the
first manifest cannot be replayed. HTTP/TLS connection attempts and DNS resolution
need actual bounded connect/read deadlines, not just a loop deadline checked
after a potentially unbounded connect.

Current ownership runs a bounded cleanup reconciler independently of active
builds. Persist cleanup requested before exact-ID TerminateMicrovm; retry only
that exact identity with bounded backoff and inspect its state. API success alone
is not cleanup completion. A single eventually consistent not-found/error is
not automatically definitive absence. Persist observations and keep capacity
until an exact successful Get response reports TERMINATED for the bound identity
(with its termination timestamp retained), or an operator explicitly resolves it.
The API documents this state and timestamp. Automatic 404-only release is not
part of the first release; no undocumented tombstone-retention rule is assumed. IAM denial, throttling, unavailable region and timeout
leave cleanup pending and alertable; no catch-all success. Never terminate all
instances of an image or act on list/time resemblance. Known ID and original
account/region cleanup context must survive backend disablement.

On lease loss, close sockets and halt fresh AWS/store operations; in-flight
operations may still complete. A stale process with a late successful response
cannot write under its expired generation. If still alive, it may offer that
bounded exact response to the current owner through a future authenticated
handoff; do not require this optional optimization for correctness. Without
handoff, ambiguity remains. A stale process must not issue compensating terminate
based on stale state. Current owner first reconciles exact gateway outputs, then
fails incomplete builds and assumes all durable cleanup obligations. This is
single-active fail-closed operation, not active/active handover.

## Concrete transfer reuse boundary

Source inspection of `nomad/callback_service.rs::serve_connection` and
`receive_build_outputs` shows that listener admission, Nomad proof/allocation
verification, manifest creation, transfer, durable completion and metrics are
composed together. `OutputImport::finish` directly calls typed `add_to_store_nar`
and checks validity; it does not call `store/promotion.rs` validation/promotion.
Do not assume extracting that entire callback gives the MicroVM contract. This
is a design boundary observation, not a qualification or remediation of Nomad.

Use `store/promotion.rs::validate_and_promote_nar` and the gateway promotion
backend for MicroVM outputs, with admitted output authority supplied separately
from worker metadata. Verify hashes, sizes, references, deriver and content
address through the owning store interfaces, then record authoritative outputs
before terminal success. The existing `nomad_output_session.rs` tests exercise
real protocol-state logic (not storage); `store_promotion/real_store.rs` has a
real mutation rejection contract and an explicitly ignored matching-promotion
case because its private store namespace differs. Neither substitutes for a new
real MicroVM gateway-store integration contract. Do not blanket-enable the
ignored test or count its existence as passing coverage.

Smallest initial reuse candidate is authenticated `WorkerSession` transfer/build
logic with a separate MicroVM connector and credential admission. Inspection of
`connect`/`connect_once` confirms Nomad authentication and its named WebSocket
subprotocol are sent there; do not route MicroVM bootstrap through that connector.
`receive_authenticated_manifest` accepts an already connected socket, but initializes
Nomad protocol-session state internally and uses fixed path/input limits. Its
connection deadline begins after manifest receipt, so pre-manifest waiting needs
an independent bounded deadline. These are concrete adaptation points, not an
assertion that the current public helper is a complete production worker API.
Preserve the
TLNW Nomad wire contract and its existing callers. Before moving protocol types,
read their entire implementation and worker callers, add behavioral parity tests,
and decide whether a backend-neutral transfer module is actually necessary.
Remaining full-file reads are an implementation prerequisite; this ADR does not
claim a completed extraction design. No Nomad execution identity crosses the
MicroVM boundary.

## Configuration and deployment responsibilities

Add a distinct optional MicroVM backend collection using the existing
`service/config/{raw,model,validation}.rs` strict decoding/validated-model pattern.
Proposed fields are domains, not a final TOML API:

- Backend name, `aarch64-linux`, explicitly qualified system features, selection
  priority and concurrent execution capacity.
- Expected AWS account, explicit region, exact image ARN/version, fixed approved
  resource shape, provider maximum lifetime and idle policy. Client derivation
  environment never selects these. Reject inconsistent image/account/region.
- Bounded provider request/read timeouts, polling/backoff, launch rate and
  unresolved-intent limits; no build-retry field.
- Dedicated callback listen address, public WSS URL, protected TLS proxy hop,
  bounded pending/active connection counts and authentication/rendezvous deadlines.
- Transfer/staging limits, disk reserve, build/collection/whole-session deadlines
  and trust-root file references. No bearer/shared secret in TOML.

Validate duration relationships and overflow, finite/nonzero capacities, shape
compatibility, URLs and cross-backend name uniqueness. Do not accept a generic
SDK endpoint override in ordinary production config: redirecting credentialed
control-plane requests is a security boundary. Test-only local provider targets
must be isolated from production credentials/state. Pin the backend configuration
fingerprint in each intent; reject unsafe in-place changes while unresolved
obligations exist. Changing image/account/region under a name cannot erase cleanup
routing. Keep old exact cleanup context usable or require resolution before the
configuration transition. No new SIGHUP semantics are implied.

The operator provisions and owns images/versions, snapshot privileges, VPC/subnet
and security controls, DNS/certificates/TLS proxy, IAM, credentials, monitoring
and cost alarms. Telchar only launches and terminates executions from this
configuration; it does not create images, buckets, networks, roles or fleets.
Use a gateway-only AWS credential provider with refresh and least privilege for
launch/exact inspection/termination and necessary image inspection. Pin account
identity at startup; fail closed on mismatch. Review exact IAM action/resource
support against the provider authorization reference before shipping a policy;
do not publish guessed ARN wildcard policy as least privilege. Worker hook
payload contains no gateway AWS credential. If guest roles are unavoidable for
platform operation, restrict them to the minimum independently audited service
needs; build sandbox must not inherit metadata credentials or hook binding files.

Permit worker egress only as needed for gateway DNS/TLS and explicitly configured
fixed-output fetch policy; ordinary builds get Nix sandbox network restrictions.
No inbound worker listener or public gateway native TLS implementation is
introduced. Build users cannot read bootstrap capabilities, daemon credentials,
store signing keys or host sentinels. Proxy body/frame/idle limits and access-log
redaction are deployment contracts that need executable coverage.

Expose bounded metrics for launch outcome, authentication rejection class,
phase latency, active/quarantined reservations, cleanup age/failure, validated
input/output bytes and deadline failures. Backend name and finite reason labels
are sufficient; intent/provider/path/subject identities belong only in protected
structured events, not metric labels. Never emit hook payload, bearer/verifier,
AWS credentials, NAR bytes or build-log bodies into control-plane records.

## Implementation slices and qualification gates

Every behavior change begins with a failing behavioral test, confirmed red, then
a minimal implementation and green rerun. These are proposed slices, not work
orders or implementation authorization:

1. **Durable identity and reservations.** Append the next migration with ownership
   triggers for intents, reservations and resolution audit records; register in
   `persistence/migrations.rs`. Real PostgreSQL tests cover fencing, unique intent,
   atomic dispatch grant/reservation, reconstruction, terminal build versus live
   cleanup, concurrent release, stale administrative generation and restart at
   every transaction boundary. No AWS calls in database tests.
2. **Exact provider control.** Separate bounded launch/get/terminate operations
   from scheduler composition. Test serialization and SDK retry configuration
   through a deterministic local HTTP server that records actual requests and
   can drop a response; that is fault injection at the real HTTP boundary, not
   proof of AWS semantics. Confirm exactly one dispatch on lost response, no
   guessed listing binding and durable quarantine. Then separately qualify the
   pinned SDK and actual provider with explicit resource/cost authorization.
3. **Image and authenticated session.** Reproducible ARM image/startup, runtime
   binding, bounded early rendezvous, single-use fenced admission and a separate
   connector. Real loopback sockets/TLS plus isolated database exercise early
   connection, wrong/expired/replayed capability, wrong ID/config, concurrent
   admission, cutoff, ownership loss and no manifest before API binding. Do not
   infer provider identity attestation from a bearer-plus-ID handshake.
4. **Transfer and completion.** Real isolated Nix stores, production promotion,
   classic and fixed-output builds. Cover missing-input transfer, corrupt NARs,
   reference/content-address mismatch, declared/actual byte bounds, incomplete or
   extra outputs, multi-output order, bounded logs, disk reserve/exhaustion and
   receipt loss. Assert invalid outputs never become authoritative success.
   Kill gateway before/after each import and completion transaction; exact
   complete outputs may win recovery, partial outputs cannot imply success.
5. **Routing and lifecycle.** Add explicit configured backend and OutputOnly
   capability, preserving fair queue/coalescing/subject quotas and no retries.
   Real PostgreSQL plus local worker processes test cancellation, client detach,
   deadlines, shutdown, stale owner, cleanup failure and administrative resolution.
   Keep known-ID cleanup runnable independently of terminal shared-build state.
6. **Deployment and stock-client qualification.** Validate NixOS configuration,
   protected proxy hop, IAM review, credential rotation, snapshot process/crypto
   inventory and sandbox secrets/network isolation. Use real stock Nix clients
   against the gateway with authorized AWS ARM executions, including substantial
   compilation and multiple outputs, provider expiry, actual ARM authentication
   and corruption negatives, restart/output-only recovery and exact cleanup.

Existing successful native ARM CMake compilation, validated outputs and emulated
produced-tool usability are feasibility evidence, not a passing production gate.
Before claiming production readiness, obtain worker peak memory/disk and gateway
staging observations, deliberate disk exhaustion, provider expiry behavior and
actual billing/cost reconciliation. These accepted experimental gaps are future
qualification requirements, not reasons to repeat the closed campaign without
permission. Include snapshot security sign-off, clean failure logs and independent
cleanup verification. Expensive/live qualification needs a separate agreed budget,
account/region/resource ledger and cleanup authority.

## Remaining proposal sections

Full owner/test audit, lifecycle detail, critical alternatives/invariant review
and approval decisions remain pending. This document is not an implementation
authorization.
