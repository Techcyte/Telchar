# Demand-driven MicroVM workers and per-build stores

## Outcome

A bounded MicroVM pool reuses local inputs across sequential derivations. Workers
accept one active derivation at a time. When the queue empties, idle workers drain
according to operator retention policy. No EFS dependency is required.

This document defines the implementation target, not implemented behavior.

## Policy

- Maximum workers bounds starting, ready, busy, draining and unresolved launches.
- Idle timeout bounds retention after a clean job reset. Zero retires immediately.
- Minimum warm workers defaults to zero; a configured floor cannot exceed capacity.
- Maximum worker lifetime bounds the entire provider instance, including idle time.
  A job is admitted only if its full execution and cleanup budget fits before expiry.
- Maximum jobs per worker optionally bounds reuse; exhausted workers drain.
- Retained-store size bounds local input growth. Disk admission precedes imports;
  garbage collection runs only between jobs, with no mounted job overlay.

Pool policy must not silently lengthen provider lifetimes or bypass monthly quota
reservations. Capacity and spend belong to the worker lifetime, not each assignment.

## Durable authority

Separate provider-worker identity from build-assignment identity. Persist each
worker's backend, deployment identity, owner generation, launch obligation, expiry,
state and current assignment. Bind provider ID only from a successful launch reply.
Ambiguous launch remains quarantined; never launch a replacement under the same
capacity reservation merely because no callback arrived.

Assignment claim and idle retirement compete in one fenced database transaction.
Only an idle worker can become assigned; only an idle worker can become draining.
Draining is irreversible and cannot accept work. Starting and draining workers
continue consuming capacity until exact termination is observed.

On ownership loss, close admission and terminate known workers. Restart recovery
remains output-only for interrupted builds; do not adopt or replay their execution.
Idle workers from a prior owner are drained rather than trusted for further jobs.

## Protocol and runtime

A provider instance opens an authenticated worker control channel. Each assignment
has its own identifier, bounded deadline and capability, distinct from launch
credentials. Stale, duplicate and foreign assignment messages fail closed.

An assignment starts a fresh execution PID/mount namespace, private Nix daemon,
private metadata and disposable writable overlay. The retained input store is the
read-only lower. Existing bounded input/output transfer and gateway verification
remain authoritative. Completion does not mark a worker idle until process teardown,
store connection closure, overlay unmount and credential reset succeed.

On cancellation, disconnect, malformed protocol or uncertain reset, drain the worker.
Never infer successful cleanup from the worker's build result alone. No automatic
build retry is introduced.

## Retained inputs

Populate missing verified inputs before mounting the job overlay. Freeze the lower
for the duration of the assignment. Preserve canonical /nix/store paths and lower
metadata through Nix local-overlay; do not share writable SQLite state.

After verified gateway output collection and clean teardown, accept retained paths
through the trusted store import boundary. Do not copy arbitrary writable-upper
contents into the retained store. Failed or unverified outputs are discarded.

GC, additions and promotion occur only when no assignment uses the lower. Reuse is
sequential inside a worker; concurrency comes from independent workers.

## Optional cross-worker input tier

Retained local inputs are the initial backing store, not a requirement that every
worker download every closure independently forever. Preserve a future second input
tier populated by Telchar, without making it a dependency of warm-worker delivery.

Candidate layouts include image-baked runtime inputs, a shared immutable input tier,
a worker-local retained tier, and a disposable per-job writable upper. Filesystem
layers and Nix metadata sources must describe the same store paths and references.
Do not add generic storage interfaces until a qualified backing implementation needs
them, but keep assignment transport independent of how retained inputs are populated.

EFS is one measured candidate, not the selected production shared tier. Alternatives
worth qualifying are immutable SquashFS/EROFS generations fetched or demand-mounted
over object storage, a Nix-aware lazy filesystem such as Snix with local chunk cache,
and local replication/prewarming from a binary cache. Binary-cache replication still
copies closure contents into each worker; it is not a shared filesystem. Lazy mounts
can avoid reading entire closures, but add metadata, availability and trust concerns.

A mounted Linux OverlayFS lower must remain unchanged. A Telchar publisher therefore
needs immutable generations and reader lifetime protection, or another filesystem
with an explicitly qualified consistency model. Publication, metadata integrity,
reference retention and GC must be designed together; do not mutate a live lower.

Selection requires representative closure size/access measurements, cold and warm
compile comparisons, concurrent-reader tests, failure behavior, sandbox compatibility,
credential handling and total operating cost. No shared-tier implementation or EFS
production rollout is approved by this extension point.

## Acceptance and verification

A1. Multiple sequential derivations use one provider instance; the second input
transfer omits retained valid paths. Both outputs are verified in the gateway.

A2. Fresh per-job sandbox, namespace, daemon, upper and credentials prevent job state
from leaking. Lower inputs remain valid, immutable during builds and reusable.

A3. Queue demand, idle timeout, warm floor, maximum jobs, lifetime and disk policy
control actual dispatch/retirement. Concurrent claim versus retirement cannot double
assign a worker or dispatch to a draining worker.

A4. Cancellation, callback loss, daemon restart, ownership fencing, reset failure,
ambiguous launch and provider expiry preserve exact cleanup/capacity obligations.
Quota accounting reserves the worker lifetime once, including idle exposure.

A5. Configuration, operator inspection, image/runtime packaging and documentation
match the connected implementation. No backward compatibility layer is implied.

Final gate: failure-first behavioral tests, real isolated PostgreSQL/Nix lifecycle
qualification, workspace checks and packaged MicroVM VM tests. Live AWS qualification
is separate and must identify the exact image and simulated dependencies, if any.

## Experimental evidence

Local nested Nix overlays and two concurrent MicroVM readers of an immutable EFS
lower passed sandboxed builds. Larger synthetic C++ tests showed approximately 26%
warm EFS compile overhead within each worker. These support a local retained store
as the initial design; they do not qualify production pooling or cross-job isolation.
