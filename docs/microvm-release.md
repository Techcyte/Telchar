# MicroVM production completion gates

This is a bounded completion plan, not a production-readiness claim. The
[backend design](adr/microvm-backend.md) records the accepted snapshot risk. Historical evidence and
limitations remain in [MicroVM operations](microvm.md). The exhausted delivery
loop is not restarted by this plan.

## Baseline and evidence reuse

Workspace qualification 197 passed 908 top-level tests, with 12 existing ignores,
plus its nested helper, formatting, daemon compilation, all-target Clippy and
check. The iteration-200 integrity review found only documentation differed from
that snapshot. Those results cover that source snapshot, not image construction,
AWS restoration, deployment, or every requirement below. No tests were rerun for
this audit.

Do not repeat the workspace suite until an integrated implementation milestone
changes executable sources. Reuse existing behavior contracts when they exercise
the same production path; add tests for missing behavior or consequential boundary
gaps, not every conceivable fault permutation.

## Confirmed implementation gaps

| Gate | Source evidence | Completion condition |
| --- | --- | --- |
| Image prerequisites | `execution.py::main` passes `lambda: True` as readiness to `runtime.make_server`. Entrypoint registers the Nix closure, but the hook itself does not verify static prerequisites. | Missing/unusable image commands, trust or binding prerequisites fail readiness without starting daemon/worker processes before capture. Accepted run still requires protocol-level daemon readiness. |
| Staging disk admission | `microvm/outputs.rs::declare` creates a staging tempfile after protocol declaration; promotion also stages bytes. Neither this collector nor its session accepts the gateway disk-reserve policy. Generic ingress build admission does not establish capacity on this separate staging filesystem. | Apply the existing disk admission policy to actual staging/promotion needs; insufficient capacity rejects before accepting the output body, with no success receipt or authoritative success. |
| Image correspondence | Config verifies ARN syntax/account/region and STS identity. Provider launch passes pinned image/version; administrative attachment checks instance metadata. These are not startup inspection of the approved image/version, resource class or network policy. | Verify the required provider metadata against operator-owned policy before selection. Keep image/network creation outside Telchar. Do not infer egress authorization from STS or a callback address. |
| Lifecycle observability | Shared gateway instruments exist; MicroVM modules do not emit the ADR's launch, admission-class, quarantine, cleanup-age/failure and phase instruments. | Emit bounded instruments at actual lifecycle boundaries, including retained obligations; verify through a real metrics receiver. No intent/provider/path labels. |

This is a confirmed-gap list, not a claim that all remaining configuration and
runtime behavior has been exhaustively reviewed. Selection priority/features and
launch/pending bounds must be reconciled against the accepted checklist during
the policy integration milestone; ADR proposed field names are not automatically
additional required APIs.

## Accepted snapshot risk

Travis accepts residual snapshot/PRNG risk as documentation-only, not a release
blocker. Keep the AWS base and fresh daemon/worker startup after `/run`. Nix-store
OpenSSL differs from the AWS-patched system library; no snapshot-uniqueness proof
is claimed. No further crypto investigation, process/library inventory campaign,
custom OpenSSL packaging or external sign-off is required for this concern.
Actual image/runtime functionality still needs qualification; live AWS work
requires separate resource/cost authority.

## Bounded execution order

1. **One integrated policy/runtime change set.** Close confirmed prerequisite,
   staging admission, image-policy and observability gaps, preserving existing
   scheduling, cleanup and trust boundaries. Each behavior gets a focused red/green
   contract. Exit: these production paths work together; targeted tests and lint
   pass. Any additional architectural change is reported before implementation.
2. **One consequential qualification campaign.** Exercise configured daemon and
   dedicated worker with real stores/TLS/PostgreSQL, including distinct-build
   fairness/quota/concurrency, recursive fixed outputs, invalid input/transfer
   limits, cleanup across removed configuration, and NixOS runtime wiring. Reuse
   already-covered corruption, exact binding, fencing and recovery contracts.
   Add a missing case only where it establishes an accepted requirement not
   exercised by an existing production-path test.
3. **Release candidate verification.** Build the actual selected image artifact;
   qualify its native runtime and stock-client boundary; then run the full
   workspace and exact fresh-shell verification once for that candidate. Record
   source/artifact identities, commands, prerequisites, resource caps, complete
   logs, failures and ignored tests. Separate local completion from unauthorized
   live AWS gates.

These are checkpoints, not guaranteed short coding sessions. If a checkpoint
uncovers another independent subsystem redesign, report its delivery impact
instead of growing this plan automatically.

## Administrative testing boundary

The absolute request budget, shutdown interruption, ownership fencing, immutable
audit and conservative unknown-outcome CLI behavior have focused evidence.
Persistence qualification already demonstrates that lost COMMIT responses can
hide committed state. This supports retaining the conservative CLI contract;
it does not require inventing a precise wire-level certainty API to ship.

Remaining task 28 stays open. Prioritize an integrated lost-response/audit check
and any demonstrated unbounded operation. Additional TLS-establishment variants,
nested-scope combinations and listener-race permutations are not automatically
release blockers merely because they are imaginable. Defer optional cases only
through explicit scope review, never by marking the entire task complete.

## Authority and preservation

No deployment, live AWS calls, paid model use, crypto replacement, resource-cap
increase, artifact deletion or replacement autonomous loop is authorized by this
plan. Preserve prior JJ changes, evidence, VM disks, keys, caches, GC roots and
rerun artifacts. Task 13 remains incomplete until local acceptance actually passes;
production readiness additionally requires applicable provider/security gates,
with the snapshot/PRNG concern explicitly accepted above.
