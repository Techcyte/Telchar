# MicroVM production completion gates

This is a bounded completion plan, not a production-readiness claim. The
[backend design](adr/microvm-backend.md) records the accepted snapshot risk. Historical evidence and
limitations remain in [MicroVM operations](microvm.md). The exhausted delivery
loop is not restarted by this plan.

## Baseline and evidence reuse

Local acceptance completed at finalization 25. Integrated verification 22 passed
917 top-level tests plus one nested helper, with 12 existing ignores, formatting,
all-target Clippy, workspace check and daemon compilation. It also qualified the
selected ARM64 OCI runtime with a sandboxed 512-function GCC build, independent
input transfer, gateway validation/registration and stock-client copyback.
Reconciliation 24 and read-only integrity review 25 closed local task 13.

That evidence applies to source
`29db0719cd1c9f3ac4c0acc9717158f6d448f58641a3d8a31964f843ab181fa2`
and its selected artifacts. It does not qualify subsequent VPC configuration
changes or the production AWS provider path. The separate configured AWS smoke
below qualifies one successful execution at `cd112dc1`; it does not rerun that
workspace gate. The completed loop stays closed.
The sections below retain historical qualification boundaries; they are not an
open local task list.

Do not repeat the workspace suite until an integrated implementation milestone
changes executable sources. Reuse existing behavior contracts when they exercise
the same production path; add tests for missing behavior or consequential boundary
gaps, not every conceivable fault permutation.

## Configured AWS compiler smoke

One real production-provider execution passed with gateway source `cd112dc1`,
exact image version `1.0` and a version-qualified VPC egress connector `:1` in
`us-west-2`. AWS's exact-ID response independently returned that singleton
`egressNetworkConnectors` selection. The ARM64 image used OCI manifest
`sha256:65b5bba3a5a8d7ba3f303f0e86d4758b6617a77dbd034cdefe792ae2b1cd296f`,
a uniquely generated callback certificate and the retained packaged worker
`a11b1aeb57c3efd24a4b1bff554cda0ecbc22ae59f69313323228f991cc69256`.
The worker was not rebuilt from the gateway revision; its runtime recipe and
sandbox policy were unchanged.

A stock Nix client used OpenSSH/`ssh-ng` to an isolated local gateway with private
PostgreSQL, store and spool. The selected worker reached it through authenticated
TLS/WSS, a narrow private relay and reverse SSH. GCC 15.2 compiled and linked 512
C functions and ran an arithmetic check under the nonroot sandbox; binding and
runtime source were invisible to the builder. Compiler prerequisites were
registered in the gateway store, not preloaded into the worker image. Gateway
validation/registration and independent client copyback returned exactly
`dedicated-worker-success\n` (25 bytes). The gateway execute phase took 105.107
seconds, including launch, callback, transfers and build; this is not a boot or
throughput benchmark.

Exact VM `microvm-0e1fcfa5-6f2f-33fd-b653-cdb82228b052` was observed
`TERMINATED`. Retained PostgreSQL independently showed shared-build `succeeded`,
launch `closed`, released reservation and recorded termination observation.
Exact cloud image, connector, ECR repository, artifact bucket, roles, log group
and connector security group were deleted and independently checked absent.
Relay rules/listeners were removed; appliance ports 22, 2222 and 7443 remained.
Local/native evidence, images, keys, stores and failed attempts were preserved.

Evidence is retained outside the repository under
`~/.local/state/telchar-microvm-test/`: `vpc-real-relay-build.log`,
`vpc-gateway-t23g1r0c/`, `vpc-live-qx3lkyw4/` and `vpc-final-verify.log`.
Preparation failures, including an unregistered host-owned store collision,
missing SDK SSO support and a client GC-root alias rejection, remain failed
attempts rather than being counted as successful runs. The live harness supplied
existing temporary credentials in-memory; it did not add SDK SSO support.
CloudWatch capture contained image-build logs, not worker-runtime logs. The
worker/compiler evidence comes from the authenticated build transaction and
retained client/gateway/durable-state records.

This closes one successful configured AWS smoke and exact cleanup, not live
failure/restart/fencing/concurrency, resource sizing, billing, OTLP export or
production readiness. Historical full-workspace evidence remains source-specific.

## Confirmed implementation gaps

| Gate | Source evidence | Completion condition |
| --- | --- | --- |
| Image prerequisites | `execution.py::image_ready` checks commands, trust and binding destination; HTTP-process regression covers rejection without process startup. | Implemented and focused tests passed. Accepted run still requires protocol-level daemon readiness; native image qualification remains separate. |
| Staging disk admission | `microvm/outputs.rs::declare` applies existing disk policy before declaration/tempfile creation, accounting for two staging copies and store import. | Implemented; focused boundary, rejection and real-store contracts passed. Measurement is not an atomic reservation against unrelated writes; existing probe-failure policy remains unchanged. |
| Image correspondence | Config verifies ARN syntax/account/region and STS identity. Startup also inspects exact image/version, successful/active state and ARM64 architecture. Provider launch pins image/version; attachment checks instance metadata. Resource sizing remains to be qualified. | Verify execution compatibility and qualify resource sizing. Each policy requires an explicit same-account/region VPC egress connector ARN, sent on every launch and included in configuration/request digests. Provisioning, routing and security-group review remain operator-owned. One version-pinned connector completed the configured AWS smoke above; other operator network policies are not attested. |
| Lifecycle observability | Launch response, cleanup result, callback admission and rendezvous count/duration are instrumented at actual boundaries with bounded outcomes. Cleanup unchanged includes unresolved and already-resolved no-ops; rendezvous connected does not establish build success. Cleanup scans also sample retained obligation counts and oldest reservation age, separated by identified/unidentified provider identity. These histograms aggregate scan observations, not fleet-wide current gauges or time since cleanup became eligible. Worker phase durations, deadline observations, validated input bytes and acknowledged output bytes have bounded receiver evidence. | Emit bounded instruments at actual lifecycle boundaries, including retained obligations; verify through a real metrics receiver. No intent/provider/path labels. |

This is a confirmed-gap list, not a claim that all remaining configuration and
runtime behavior has been exhaustively reviewed. Selection priority/features and
launch/pending bounds must be reconciled against the accepted checklist during
the policy integration milestone; ADR proposed field names are not automatically
additional required APIs.

## Local integration evidence boundaries

The configured NixOS contract in `nix/checks/nixos/microvm.nix` exercises
independent stock-Nix clients, SSH identities mapped to Alice and Bob, real
PostgreSQL, TLS/WebSocket transport and sandboxed workers. Provider responses
are simulated. It requires three distinct successful outputs, Alice's active
and queued limits, rejection of an overflowing request, Bob's admission while
Alice is queued, Alice's dispatch FIFO, nonoverlapping physical reservations,
exact observed worker termination and restart through migration 24.

Round-robin applies to scheduler admission, not provider dispatch order.
`subject_rotation_survives_scheduler_restart` in
`crates/telchar/tests/shared_build_scheduler.rs` exercises the actual scheduler
with persisted rotation and competing queued subjects. Together these contracts
cover rotation and configured quota/FIFO/capacity composition; the configured
VM does not establish competitive round-robin dispatch order.

The configured-daemon worker test also verifies acknowledged launch, connected
rendezvous, accepted admission, duration samples and bounded attributes through
a real OTLP receiver after validated output success and shutdown. Separate real
listener/PostgreSQL/OTLP contracts exercise protocol, bootstrap and authentication
rejection stages. Other finite rejection stages are
implemented but do not yet have direct receiver qualification. These contracts
do not close every phase instrument.

The VM's staging observer includes the exact daemon's anonymous/deleted open
files and deduplicates inode identities. A 16 MiB output transfer observed
33,554,704 logical bytes and 33,562,624 allocated bytes at the largest sample,
with zero at the beginning and end. These are sampled staging observations,
not whole import-store disk usage, representative compiler sizing, true peaks
or billing measurements.

A separate native ARM64 runtime contract compiles and links 512 generated C
functions with GCC 15.2, executes an arithmetic check and returns the expected
output through gateway registration and stock-client copyback. The compiler
closure is a declared derivation dependency, registered only in the gateway
prerequisite store and transferred into the independent worker store. The
worker remains nonroot and sandboxed. Current gateway/test sources were compiled
for this run; the packaged worker and AWS context were retained artifacts, not
rebuilt from that candidate.

Worker-runtime-only observation collected 163 samples over 21.13 seconds at a
nominal 100 ms interval plus sampling cost. Maximum sampled aggregate PSS was
123,815,936 bytes across at most nine observed processes. Maximum sampled
allocated bytes were 331,894,784 in the writable store, 9,277,440 in Nix state,
4,096 in runtime state and zero in the tracked temporary directory. Writable-store
usage includes imported compiler dependencies, not just compiler scratch. These
are per-category sampled maxima, not necessarily simultaneous, true cgroup peaks,
a representative application sizing guarantee or provider billing measurements.
The compiler command's enclosing 8 GiB qualification scope is not a worker memory
requirement. Whole gateway import-store disk usage remains unmeasured.

Integrated finalization verification closed the local fresh-shell gate and
required phase instrumentation. Direct report-failure export assertions, negative
byte-counter cases and image OTLP export remain disclosed coverage gaps, not
claims made by that gate. Integrated runtime observation also captured live
gateway store boundaries: 84 to 87 registered paths, with 16,384 allocated and
5,864 logical bytes added. Boundaries are not peaks or compiler-import sizing.

## Operator-owned infrastructure

Telchar validates execution compatibility and concerns itself with resource sizing.
It requires an explicit VPC egress connector ARN and checks its
syntax, account and region. It does not attest connector readiness, routes or
security-group policy. Pinning image/version and connector identity does not
audit network policy; operators own that infrastructure independently. Qualified
ARNs pin connector versions; unqualified ARNs follow AWS updates. Resource
qualification must distinguish baseline
memory from elastic peak capacity and disk availability, not invent a hard RAM cap
or require duplicate configuration merely to mirror provider metadata.

## Accepted snapshot risk

Travis accepts residual snapshot/PRNG risk as documentation-only, not a release
blocker. Keep the AWS base and fresh daemon/worker startup after `/run`. Nix-store
OpenSSL differs from the AWS-patched system library; no snapshot-uniqueness proof
is claimed. No further crypto investigation, process/library inventory campaign,
custom OpenSSL packaging or external sign-off is required for this concern.
Selected-image native runtime functionality passed locally. The configured AWS
smoke above also exercises image restoration and one successful provider lifecycle;
live failure, restart, fencing and concurrency qualification remain separate.

## Bounded execution order

1. **One integrated policy/runtime change set.** Close confirmed prerequisite,
   staging admission, execution compatibility, sizing and observability gaps, preserving existing
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

This document grants no deployment, paid model use, crypto replacement,
resource-cap increase, artifact deletion or replacement autonomous loop authority.
Travis separately authorized disposable AWS qualification using `hub-admin` in
`us-west-2`, at most ten concurrent MicroVMs, initially one with a 600-second
maximum lifetime, and exact cleanup of owned test infrastructure. The private
reverse-SSH relay first passed a nonce probe, then carried the configured AWS
smoke above. Live failure/restart/fencing/concurrency gates remain open. Preserve prior JJ changes, evidence, VM disks, keys, caches, GC roots and
rerun artifacts. Task 13 is locally complete; production readiness remains a
separate claim, with snapshot/PRNG risk explicitly accepted above.
