# Managed MicroVM build execution

## Status

Proposed; incomplete design under review. Not accepted or implemented.

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

## Remaining proposal sections

Durable state/crash matrix, authentication/transport, lifecycle/recovery,
configuration and deployment, implementation slices, qualification gates,
alternatives and approval decisions are pending. This document is not an
implementation authorization.
