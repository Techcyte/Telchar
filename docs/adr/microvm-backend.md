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

## Design questions to resolve

1. Launch-response loss: callback-assisted authenticated binding versus terminal
   ambiguous intent retained until the provider lifetime bound. No list/time
   guesses or automatic replacement launch. Establish token replay guarantees
   before proposing launch replay.
2. Early connection: admit bounded authenticated bootstrap before provider ID
   commit without admitting the build or trusting a self-reported ID alone.
   Determine exact binding checks and transactional admission sequence.
3. Recovery: separate monitoring a known provider execution from reconnecting
   its transfer stream. Existing worker protocol does not establish durable
   resumable transfer. Define fail-closed first-release behavior explicitly.
4. Ownership loss: AWS cannot enforce PostgreSQL generation. Specify stale
   side-effect exposure, launch deadline and cleanup handoff; do not claim
   database triggers fence remote API calls.
5. Snapshot safety: retained Nix daemon ordinary OpenSSL is unqualified. Compare
   fresh post-restore daemon startup against snapshot-safe dependency packaging;
   image readiness and exact startup/sandbox checks need a coherent policy.

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
  bearer bytes, NARs or logs. Secret persistence policy remains a design question.

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
| Prepared but not dispatching | Current owner may grant the single dispatch, after checking cutoff and cancellation |
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

Stale owners must stop callback/store activity and must not terminate an adopted
execution based on stale cancellation state. Exact cleanup belongs to current
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

These decisions constrain the remaining design. Operator reconciliation still
needs a concrete, auditable workflow; approval is not permission to discard an
unknown execution or terminate unrelated resources.

## Remaining proposal sections

Credential durability and callback transport extraction, capacity quarantine,
configuration/deployment, image startup, implementation slices, qualification
gates, alternatives and approval decisions remain pending. This document is not
an implementation authorization.
