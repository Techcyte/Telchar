# Lambda MicroVM backend experiment

**MicroVM integration is experimental, not production-ready.** Historical disposable
fixtures exercise authenticated WSS, NAR transfers and validated output
registration locally and on actual AWS ARM64 MicroVMs. Partial production
implementation is described below; its durable ledger is included in gateway
schema migrations. Daemon configuration/startup and routing consume the execution
service; output-only recovery never adopts or relaunches MicroVM executions.
AWS Lambda MicroVMs are distinct from ordinary Lambda functions.

## Production implementation status

The [backend ADR](adr/microvm-backend.md) defines the intended conservative release.
Implemented boundaries include:

- Migration `0019_microvm_launches.sql` and fenced reservation, single dispatch,
  exact provider binding, single-use admission and cleanup-recording APIs.
- Bounded provider launch/get/terminate with SDK launch retries disabled. Ambiguous
  launches retain capacity; only an exact observed `TERMINATED` releases a known
  execution's cleanup reservation.
- Separate `/microvm` WebSocket admission with expiring verifier-only credentials,
  authenticated early waiting and pending-connection bounds. TLS termination is
  operator-owned; Nomad authentication remains separate.
- Gateway sessions reconstruct manifests from exact persisted admitted requests,
  transfer bounded inputs, validate output authority/NARs/references/content
  addresses, and require gateway registration before success. Dedicated-worker
  contracts cover classic and multi-output builds with real TLS, PostgreSQL and
  isolated canonical stores on x86-64; sandboxing is disabled in these fixtures.
- Exact-intent handoff, bounded callback admission, active transport interruption
  on cancellation/ownership loss, and an owned execution/cleanup service with
  shared scheduler handles and fail-closed lifecycle health. Independent cleanup
  rotates bounded batches without releasing uncertain reservations.
- Strict `[[backends.microvm]]` deployment policies with explicit
  `backends.microvm_bind` for the operator-proxied callback listener. Daemon
  startup verifies providers before attaching services; unattached/unhealthy
  targets cannot execute. All policies share bounded admission and shut down
  together, with one cleanup lifecycle per exact account/region shared by aliases.
  Policy changes require restart.
- Validated deployment policy and configured provider construction
  using signed STS account verification. Each operation resolves its credential
  source; changed credentials require re-attestation before using that exact
  credential snapshot. Local signed HTTP
  fixtures use fake credentials; they are not AWS qualification.
- `telchar-microvm-worker` with explicit trust/store/binding inputs, bounded WSS
  transport and existing worker transfer/build mechanics.
- `deploy/microvm/execution.py` runtime hooks, protected execution binding and
  fresh daemon/worker supervision inside an execution PID namespace. Local Linux
  tests cover escaped `setsid()` descendants on deadline, cancellation, outer
  supervisor loss and namespace-helper loss. Missing namespace capability rejects
  execution before daemon, readiness or worker commands start. These tests do not
  establish native ARM sandbox or secret isolation.

These are partial components, not a deployable service. Local daemon-process
contracts cover classic, multiple and flat fixed outputs, corruption/failure,
coalescing/detachment, output-only crash recovery and independent cleanup with a
simulated provider. Reload permits share accounting; durable reservations retain
same-name or exact account/region quarantine across aliases. Startup refuses
retained obligations without configured account/region authority.

Remaining release gates include distinct-build fairness/quota qualification,
recursive fixed outputs and remaining transfer failures, staging disk admission,
store-I/O deadline/fencing guarantees, native executing-builder recovery,
provider image/egress correspondence, administrative reconciliation and audit,
metrics, NixOS wiring, and a secure ARM production image. Configuration acceptance is not a
production release claim. Mandatory
sandbox/build-user/secret isolation and snapshot cryptographic safety remain
unresolved production gates. Do not deploy the experimental images as a substitute.

Before running compilation or qualification, establish a verified hard memory
limit covering the command and its descendants. Full workspace compilation
exhausted the development host's memory with concurrent linkers. Serial test
threads and `CARGO_BUILD_JOBS=1` reduce concurrency but do not enforce memory
limits. Do not run heavy checks without resource containment.

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

The image in `nix/microvm-worker.nix` builds the example worker and uses the
experimental bootstrap composition in `nix/microvm-probe.nix`. It does not package
`telchar-microvm-worker` with `execution.py`; its daemon starts before snapshot.
Do not treat that image as the qualified runtime described above.

The AWS-base packaging path is `nix/microvm-aws-context.nix`, called with ARM64
`pkgs`, the dedicated `worker` package and public DER `trust`. Its output is a
Docker build context using `deploy/microvm/Dockerfile` and AWS's
`public.ecr.aws/lambda/microvms:al2023-minimal` base. Amazon-packaged Python 3.12
runs the lifecycle listener; the Docker build checks namespace API availability.
Dedicated build accounts are added without replacing AWS system accounts. The
existing sandbox policy and unmodified Nix/worker closure are included; Nix
daemon and worker start only after the run hook. No custom OpenSSL patch is used.

This path has derivation evaluation and local supervisor/hook regression evidence,
not a completed container build or ARM runtime qualification. The Dockerfile pins
an anonymously retrieved AWS image index digest with an ARM64 manifest. Public
Amazon's package manager successfully installs Python 3.12.14 and shadow-utils
in the extracted ARM64 base under QEMU/PRoot. That Python imports `ssl` and exposes
`os.unshare` and `os.CLONE_NEWPID`; this checks API availability, not namespace
permissions. The package transaction does not replace the base's OpenSSL.
Full container construction, actual linkage inspection and cold-start Nix snapshot
applicability still require verification. Emulation is not native sandbox evidence. AWS's base does not replace Nix-store libraries.
The isolated OpenSSL experiments below are historical investigation, not the
chosen packaging approach.

`nix/microvm-execution.nix` provides a separate ARM64-only dedicated execution
composition. Call it with ARM64 `pkgs`, a `worker` package containing
`bin/telchar-microvm-worker` (the existing `telchar-nomad-worker` package builds
that binary), and an operator-owned DER root certificate as `trust`. Trust is
public image material, not a private key or runtime credential. The composition
registers its store closure, then runs `execution.py`; no Nix daemon or worker
starts until an accepted run hook. The hook binding lives under a root-only
`/run/telchar-microvm` directory. Both image compositions consume the sandbox and
sixteen nonroot build-user definitions in `nix/microvm-store.nix`.

This composition is tagged `unqualified`, not exported as a production flake
image. Evaluation checks derivation construction only: ARM image build, actual
hook-to-worker execution, sandbox/procfs/secret isolation and snapshot/OpenSSL
qualification remain required. Neither the tag nor fresh-process startup is a
security attestation. Lifecycle `ready`, `validate`, `suspend` and `resume`
accept only idle state; completed or failed execution cannot become a reusable
snapshot through those hooks. This guard does not erase secrets from memory or
establish provider snapshot behavior. The experimental image remains separate
for historical probe reproduction.

AWS's [snapshot guidance](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
states that snapshots include process memory, disk, connections and file
descriptors, and requires AWS-patched OpenSSL for applications using OpenSSL.
The Nix closure does not yet establish compliance with that requirement. Starting
TLS/build processes after the run hook alone is not grounds to waive it.

The Amazon Linux source RPM
[`openssl-3.5.8-1.amzn2023.0.3.src.rpm`](https://cdn.amazonlinux.com/al2023/blobstore/24b99be684592c7891e7dde95f1525de96981c4abfcbe7aeb868ec5b3b098ed7/openssl-3.5.8-1.amzn2023.0.3.src.rpm)
contains `0500-snapsafe-provider.patch`; its SHA256 is
`24b99be684592c7891e7dde95f1525de96981c4abfcbe7aeb868ec5b3b098ed7`.
The RPM spec applies that patch specifically to the snapsafe subpackage, not
ordinary `openssl-libs`. The patch uses `/dev/sysgenid` and reseeding state in
OpenSSL's random-provider chain. Its initialization treats an absent device as
unsupported rather than failing. Consequently, ordinary local cryptographic
tests without that device cannot establish snapshot reseeding safety. This is
source inspection, not approval of a Nix port or proof of runtime compliance;
patch applicability, dependency linkage and restored-generation behavior still
need qualification.

The patch adds generation checks around EVP random generation/nonces and RAND
engine calls, with a ten-iteration detection limit. It declares
`RAND_snapsafe_is_supported()` in a public header, but does not add that symbol to
`util/libcrypto.num`. A native x86-64 shared build of the unchanged patch confirms
that `RAND_bytes` works but dynamic lookup of `RAND_snapsafe_is_supported` fails
with an undefined symbol. Shared consumers cannot use that declared probe to
reject unsupported devices without additional packaging/source work. Its own tests can substitute a synthetic generation counter when the
kernel device is absent, and several reseed tests use fake providers. Those tests
can qualify the patch logic, not actual snapshot restoration. The patch explicitly
does not support the FIPS module; this composition makes no FIPS claim.

Pinned Nixpkgs selects OpenSSL 3.6.3, whereas the inspected Amazon source is
3.5.8 with other distribution patches applied before the snapsafe patch. Direct
application to checksum-verified upstream 3.6.3 with `patch --dry-run --fuzz=0`
fails on two RAND engine-call hunks and one test-build hunk. The engine-call
sites include integer-range checks and casts absent from the patch context;
3.6.3 also has provider-direct random calls outside the patched EVP route.
A port requires semantic review, not merely accepting offsets. The unchanged
patch does pass the same zero-fuzz dry run against the 3.5.8 tarball embedded in
the checksum-verified Amazon source RPM (tarball SHA256
`a8f84a39918ec6415ce765d9b429d313ba97b8143169c172e734b9514464f5b2`),
without preceding distribution patches. This establishes source applicability
only; provider-direct randomness also exists in that source, so matching the
version does not establish coverage of every random-provider configuration.
A local x86-64 static, non-FIPS build of that patched source completed and
`test_rand` reported PASS for a seven-test recipe, which includes a skipped FIPS
case; the separate FIPS preparation recipe was also skipped. The added AWS test
source emitted 23 deprecated-engine/RAND API warnings and one unused-variable
warning. Build-variable generation also emitted Perl uninitialized-value and
missing-value diagnostics, so this is not pristine build qualification. It is
not ARM, shared-library, full OpenSSL-suite or real snapshot-generation evidence.

`tests/microvm/qualify-snapsafe.py ARCHIVE PATCH ARTIFACTS` provides a separate
native x86-64 shared-library experiment under the same required containment and
flake development shell. It verifies the exact 3.5.8 archive and AWS patch
checksums, extracts into a unique preserved directory, applies with zero fuzz,
and builds libraries serially. It then loads that exact `libcrypto.so.3`, calls
`RAND_bytes`, and requires the public `RAND_snapsafe_is_supported` symbol. Missing
symbol is a qualification failure, not permission to assume device support.
This experiment does not alter image dependencies or qualify restored generation
behavior. Archive and patch are operator-supplied preserved source artifacts;
the script does not download or trust arbitrary replacements. Its optional
`--export-probe` applies `tests/microvm/snapsafe-export.patch`, adding only the
missing export entry. Relinking a preserved copy with that entry makes the probe
callable and returns unsupported on a host without `/dev/sysgenid`; this is not
positive device or restore qualification. The unchanged-patch failure remains
reproducible without the option.

Pinned Nixpkgs also provides an `openssl_3_5` recipe (3.5.7), a possible starting
point for isolated matching-version packaging rather than porting random logic
to 3.6. Its first Linux patch, `nix-ssl-cert-file.patch`, fails zero-fuzz
application against 3.5.8 because surrounding certificate-load formatting
differs. Its kernel-detection patch also needs context adjustment for the
intervening Windows-store block. Qualification patches preserve both Nix changes
with matching context; these and the unchanged system-certificate-path patch
apply with zero fuzz to the AWS-patched 3.5.8 source.

`tests/microvm/snapsafe-package.nix` provides an isolated Linux recipe taking
`pkgs`, the checksum-verified `archive`, and AWS `patch`. It retains Nixpkgs
package hooks and checks, adds the probe export and context-adjusted patches,
and disables parallel building. A contained native x86 build completed checks,
installation and fixup; the upstream harness reported PASS (346 files, 4512 tests),
with disabled-feature/platform skips. Compiler, Perl generator and inherited
post-patch warnings remain, so this is not pristine qualification. The installed
CLI resolves both SSL libraries from this package; an exact installed-library
probe passes `RAND_bytes` and reports device support zero. This establishes
package construction and negative-device probing, not snapshot safety. No image
imports this recipe, and no ARM or complete image-closure linkage claim follows. `bash tests/microvm/build-package.sh DERIVATION ARTIFACT_DIRECTORY`
runs a local build using a retained copy-on-write canonical store and private Nix
database, with remote builders and substitutions disabled. It requires the
verified resource envelope and already realized build dependencies. This fixture
disables Nix sandboxing and is not production sandbox evidence. Preserve its
reported artifact directory and host dependency paths for later output inspection.

ARM derivation
inspection identifies Python 3.13.14 and OpenSSH 10.4p1 as OpenSSL consumers;
Nix 2.34.8's `nix-util` and `nix-main` consume OpenSSL, while `nix-store`
consumes curl; curl 8.21.0 propagates OpenSSL 3.6.3. Top-level `buildInputs`
alone are not a complete dependency inventory. These are package-definition and
evaluation findings, not realized ELF linkage evidence. Adding a patched library to image
contents alone does not redirect Nix store references. An image-local package-set
change would need dependency reconstruction and realized ELF/closure inspection,
followed by patch tests and actual device/generation qualification. No such
production override or realized image-linkage qualification is implemented.
`tests/microvm/snapsafe-dependencies.nix` evaluates an isolated package-set
extension and asserts exact patched-library derivation inputs for Python,
OpenSSH, curl, Nix util and Nix main, plus the replaced curl input for Nix store.
ARM evaluation succeeds for those edges. Traversing declared `buildInputs` and
`propagatedBuildInputs` from Nix, Python, coreutils and bash visits 95 derivations
and finds only the patched OpenSSL derivation. This graph excludes embedded
references, native inputs and the separately supplied worker; it is not a runtime
closure inventory, a build, or ABI/runtime validation. The experiment does not
modify the execution image or gateway packages.

The sections below describe historical experiments, not production qualification.

## What exists

- `nix/microvm-probe.nix`: a Linux container with Nix, a root daemon, sixteen
  dedicated build users, and a lifecycle adapter. `sandbox = true` and
  `sandbox-fallback = false` are mandatory. No host store is mounted.
- `deploy/microvm/runtime.py`: bounded lifecycle hook requests. `/ready` and
  `/validate` ping the daemon through the Nix client without executing a build
  before snapshot.
  `/run` starts one fixed probe asynchronously; duplicate delivery for the same
  MicroVM ID does not repeat it. Different IDs and nonempty runtime payloads are
  rejected. There is no arbitrary command endpoint. Active probes reject suspend.
- The probe builds a derivation using the daemon with substitution disabled and
  verifies a non-root builder UID, filesystem isolation, and a separate network
  namespace. The probe has a five-minute subprocess deadline.

The lifecycle status JSON reports `idle`, `running`, `succeeded`, or `failed`.
HTTP 200 from `/run` means accepted, **not** that the sandbox test passed.

## Local validation

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tests/microvm/test_runtime.py
bash tests/microvm/local-probe.sh
```

The second command requires Docker and deliberately starts a **privileged**
disposable container with networking disabled. It mounts no host directories and
removes the container on exit. Only run it on an approved development machine.
It builds for the local architecture: success on x86-64 is not ARM/AWS evidence.

Evaluate the intended ARM image (no AWS resources):

```sh
nix eval --impure --raw --expr '
  let f = builtins.getFlake (toString ./.);
      pkgs = import f.inputs.nixpkgs { system = "aarch64-linux"; };
  in (import ./nix/microvm-probe.nix { inherit pkgs; }).drvPath'
```

Replace `nix eval --impure --raw` with `nix build --impure --no-link` and remove
`.drvPath` to build the image. An ARM-capable Nix builder is required. This is
intentionally not a new public flake output or release artifact yet.

## AWS sandbox evidence

A disposable ARM64 MicroVM in `us-west-2` completed the fixed sandbox probe.
The image used the managed AL2023 base, elevated capabilities, an 8192 MiB
baseline, ready/validate hooks, and an asynchronous run hook. The test configured
an explicit 600-second maximum lifetime and no idle suspension policy.

The sandboxed daemon built
`/nix/store/iql5cx0c0sx4gs5i81hw00p6iqxx7lsz-telchar-microvm-sandbox-proof`
with substitution disabled. Its builder assertions verified a non-root UID,
invisibility of the host-only sentinel, and a different network namespace.
Authenticated endpoint polling returned `succeeded`, corroborated by the daemon
build log and lifecycle completion event. The test did not perform TLS or NAR
transfers inside the MicroVM, exercise gateway integration, or measure billing.

The image build accepted a ZIP containing `FROM scratch`, the flattened ARM
probe filesystem, and its entrypoint/environment. No ECR repository, Docker
preload images, or GitLab runner software was needed. The transferred archive's
SHA-256 was
`455d9cddfad7e5f5611a5bd7632c44d1a4af8515a8c9bb2dac90afa3a2de2886`.

The VM reached `RUNNING` by the first two-second polling interval; this is a
coarse observation, not a startup benchmark. Termination was confirmed before
deleting the image, artifact bucket, temporary role, and CloudWatch log group.

A follow-up ARM64 run used the Nix client for daemon readiness, retained the
probe derivation/output through GC roots, and explicitly disabled automatic
connectivity detection for the offline probe daemon. The real sandbox build
passed again, with no errors or warnings in the collected build/runtime logs.
Termination and deletion of all disposable test resources were confirmed.
The local disposable-container test also rejects runtime errors and warnings.
This qualifies the fixed sandbox probe only, not a production worker.

## Repeating AWS validation

Use an up-to-date AWS CLI/SDK exposing `lambda-microvms`; the development host's
AWS CLI 2.34.24 does not recognize that service. No resources are provisioned by
these tests.

In an approved test account:

1. Build the ARM image. Package a flattened filesystem and `FROM scratch`
   Dockerfile into the S3 code artifact, or use an operator-owned ECR image
   referenced by immutable digest.
2. Create the managed MicroVM image with `additionalOsCapabilities = ["ALL"]`,
   the managed AL2023 base, lifecycle hook port 8080, and the documented ready
   and validate paths. The application container is custom; the VM kernel/base
   remains AWS-managed. Configure runtime `/run` on the same port.
3. Run with a short explicit maximum duration and automatic suspension disabled.
   The probe needs no outbound connectivity. Temporarily allow the authenticated
   endpoint only for status inspection; use a short-lived token scoped to 8080.
4. Poll lifecycle status until `succeeded` or `failed`; collect bounded logs,
   resource consumption, timings, and billed snapshot read bytes. Always
   terminate the exact returned MicroVM ID, including on test failure.

Snapshot uniqueness and Nix's OpenSSL dependencies require separate review:
AWS recommends snapshot-safe OpenSSL. The probe makes no TLS connections and
must not be interpreted as validating snapshotted cryptographic state.

## Outbound WSS fixture evidence

`tests/microvm/local-exchange.py` runs the compiled `microvm-worker` and
`microvm-receiver` examples in two disposable privileged containers with separate
stores on an internal Docker network. No host store is mounted. The test supplies
a certificate trust root, execution-scoped credential and exact instance identity.
A runtime `/run` hook writes the credential binding with mode 0600 before starting
the fixed worker. The receiver authenticates before sending an admitted manifest.

A 1,040,000-byte source absent from the worker is imported through the real worker
session. Its sandbox build checks a non-root UID and an invisible host sentinel.
The receiver validates and registers the returned output before acknowledging it;
the test verifies exact source/output byte equality. Local x86 wall time was about
9.4 seconds including container setup. This is neither an ARM/AWS result nor a
transport throughput benchmark.

Run from the flake development shell after building the probe image and examples:

```sh
cargo build --locked -p telchar-nomad-worker --examples
python3 tests/microvm/local-exchange.py
```

Docker, OpenSSL, `ldd` and `patchelf` must be available. The script removes its exact
containers and network on exit. `nix/microvm-worker.nix` composes the fixed worker
and runtime adapter for ARM packaging.
Receiver output chunks are staged in an unlinked, mode-0600 file and rewound for
validation before registration. Protocol transfer limits still bound disk use.
TLS snapshot uniqueness and reconnect behavior remain unqualified.

## AWS outbound WSS qualification

An ARM64 image completed automatic AWS `/run` → outbound pinned WSS →
execution/actual-instance authentication → missing-input import → nonroot sandbox
build → production NAR validation and typed gateway registration. No Nomad
identity was fabricated. An independent x86-64 EC2 gateway had no host-store
mounts. A bounded opaque TCP relay held early TLS until `RunMicrovm` returned the
actual identity; it did not terminate TLS or bypass authentication.

Three successful runs returned the exact 1,040,000-byte source payload, including
two sandbox resource reports. The timed run checked output absence before launch
and registered a 1,040,376-byte output NAR. Observed boundaries from that single
small fixture run:

| Boundary | Seconds |
| --- | ---: |
| Launch API return | 0.278 |
| Launch to observed authenticated WSS | 2.802 |
| Manifest to input import completion / build started | 0.165 |
| Build started to output metadata | 1.437 |
| Output transfer, validation and registration | 0.081 |
| Launch through exact byte comparison | 5.073 |

Authentication observation includes API, receiver configuration/startup, relay,
TLS and polling; it is not isolated VM boot time. Input NAR bytes were 1,040,112;
effective input rate was 6.30 MB/s including negotiation/import. Effective output
rate was 12.79 MB/s including validation/registration. Neither is a network
capacity benchmark. Socket-send timing alone is not throughput.

Sandbox-visible filesystem capacity was 33,635,975,168 bytes, with 658,034,688 used
and 31,243,177,984 available. Guest `MemTotal` was 32,894,692 KiB despite the image's
8192 MiB minimum: minimum image memory is not observed runtime sizing or billing.
A failed measurement first omitted a declared coreutils input; Nix correctly hid
that tool in the sandbox. Declaring its store dependency fixed the test without
relaxing isolation.

A fresh ARM worker rejected a corrupted source NAR during actual input import and
reported failed lifecycle completion. Separately, a local pinned-TLS client to the
public EC2 receiver verified wrong execution → HTTP 403, exact binding → HTTP 101,
and disconnect → failed transfer. That receiver test is not an ARM authentication
negative run. Local real-store tests cover corrupt returned-output rejection
before registration. Reconnect, durable recovery, snapshot cryptographic uniqueness,
saturation, billed cost and production scheduling remain unqualified. Substantial
closure compilation is covered by the follow-up campaign below.

All seven test MicroVMs and the receiver were confirmed terminated. The dedicated
volume, security group, key pair, image, bucket, IAM role and log group were
independently confirmed absent. Evidence lives outside the repository at
`~/.local/state/telchar-microvm-test/wss`, including actual returned bytes/NARs,
registration metadata, phase logs, CloudWatch events and cleanup observations.
The ARM archive SHA-256 is
`63491b6daba08ed1679ad1317c136a12c6905129d7e08cabb0fc75b82503ff73`.

From the development shell, archived evidence can be checked without AWS access:

```sh
python3 tests/microvm/verify-evidence.py "$HOME/.local/state/telchar-microvm-test/wss"
bash tests/microvm/verify-local.sh
```

The first command restores each preserved NAR into a private temporary directory,
checks hashes, payloads, lifecycle events, phase ordering and cleanup records.
It does not rerun AWS executions or query live cleanup state. The second reruns
local behavioral qualification using disposable stores. Preserve the evidence
directory and probe Docker image; neither is supplied by a repository checkout.

## Substantial ARM compilation campaign

A follow-up campaign uses a uniquely named pinned CMake 4.1.6 derivation with
uncached target outputs, eight compiler jobs and mandatory nonroot/sentinel
sandbox assertions. Its realized input closure contains 109 paths and
464,615,344 NAR bytes. Actual workers requested 373,489,368 missing-input bytes;
other paths were already in the worker image. Both `out` and `debug` outputs
were validated and registered in the successful AWS run. Actual native ARM
compilation and package fixup took 425.235 seconds. Launch through registration
and authoritative metadata queries took 484.190 seconds, within the 600-second
provider limit. Output NARs were 57,537,296 bytes (`out`) and 516,599,664 bytes
(`debug`), both preserved with verified hashes and imported locally. The produced
ARM CMake configured and built a real C program; produced ARM CTest passed 1/1
and program output matched. This usability check used explicit QEMU/proot
process-tree emulation with a host compiler, not a second native ARM build.

Manifest-to-build-start took 2.273 seconds, about 164.33 MB/s effective input
transfer/import. Output transfer, validation and registration took 53.589 seconds,
about 10.71 MB/s effective. These include processing and are not isolated network
throughput. Launch API return was 0.157 seconds, not boot time. Substantial-build
worker peak memory/disk were not sampled; prior small-fixture observations do not
fill that gap. Receiver before/after filesystem and host memory observations are
retained separately.

The larger workload exposed two fixture deadlines. A real loopback WebSocket
regression reproduced read-idle `WouldBlock` during a silent build; the receiver
now retries only transient idle errors within a monotonic deadline. A subsequent
ARM run passed bootstrap linking and reached 75% of the full build before the
adapter's inherited 300-second process deadline killed the worker. This is not
evidence of failure at the AWS 600-second maximum. The execution adapter now
allows 540 seconds, leaving time before the provider limit. The fixed sandbox
probe retains its separate 300-second deadline. Both fixes have local regression
coverage; the second image version completed the same workload.

The current conservative campaign reservation is USD 40, not actual spend.
Official Oregon ARM MicroVM rates are USD 0.0000276944 per vCPU-second and
USD 0.0000036667 per GB-second. Charging a full 16 vCPU and 32 GB for 600 seconds
would cost about USD 0.336 for compute; a 32 GB snapshot read adds about USD 0.0495.
Image minimum-retention, shared receiver, storage, transfer and logs are separate.
Observed guest memory and image minimum memory do not establish billed sizing.

The fixed Rust worker starts only after `/run` writes a runtime-only execution
binding. Its rustls/ring TLS state therefore does not exist at snapshot capture.
However, the Nix daemon starts before capture and uses ordinary Nixpkgs OpenSSL,
not AWS `openssl-snapsafe-libs`. Offline store traffic avoids intended daemon TLS
but does not certify its retained RNG state. AWS's random-device guarantee and
sample uniqueness must not be represented as general cryptographic certification.
Four operator-created certificates had distinct fingerprints; these were generated
outside the guest, so they do not measure guest entropy or retained RNG state.

Failure coverage remains deliberately scoped:

| Boundary | Evidence | Limitation |
| --- | --- | --- |
| Invalid hook identity, concurrent duplicate hook, active snapshot/suspend rejection | Local HTTP runtime tests | Not an AWS replay/recovery campaign |
| Exact WSS execution binding and disconnect failure | Local real TLS receiver; prior local client to AWS receiver | Not an ARM authentication-negative run |
| Corrupt input rejected by Nix daemon | Prior actual AWS ARM execution | Small fixture, not repeated substantial compilation |
| Corrupt output rejected before registration | Local real-store receiver tests | Not an AWS ARM output-corruption run |
| Silent compiler link / bounded socket idle | Local deadline regression and successful substantial AWS compilation | Not reconnect support |
| Process lifetime | Actual ARM 300-second adapter failure; local timeout/process-group tests; successful 540-second adapter image | Not an intentional AWS 600-second provider-expiry test |
| Disk exhaustion / saturation | No induced exhaustion | Unverified; no capacity or admission-safety claim |

No extra AWS failure campaign was launched after preserving the successful build.
Production launch ambiguity, cancellation and recovery follow the backend ADR;
retries of these fixtures do not qualify that implementation.

Evidence for this separate campaign is retained under
`~/.local/state/telchar-microvm-test/workloads`. Independent AWS queries confirmed
all four workers and the receiver terminated; the volume, security group, key,
both image versions, bucket, role and log group were absent. Its ledger records
`cleanup=complete`; it must not be confused with the small-fixture campaign above.
`tests/microvm/verify-workload-evidence.py` checks archived output hashes, timing
ordering, isolation and cleanup records without AWS access. These fixtures do not
implement production launch ambiguity, fencing, cancellation, restart or
stock-client scheduling. The partial production components above are separate;
reconnect is outside the intended first release.

## Proposed integration boundary

One MicroVM per admitted ARM shared build, while the gateway may remain x86-64.
The worker should connect **outward** over authenticated WSS to the gateway.
Avoid carrying closure transfers through the managed inbound endpoint until its
bandwidth limits are measured. Endpoint activity drives automatic suspension,
so background builds must not use endpoint-idleness as their execution policy.

Do not impersonate Nomad identities with the current worker. A real backend
requires explicit MicroVM authentication and durable identity support:

- persist launch intent and an idempotency token before `RunMicrovm`;
- persist and fence exact region, image version, returned MicroVM ID and deadline;
- reconcile ambiguous launch outcomes without blindly creating another VM;
- authenticate callbacks against that exact active execution;
- retain typed manifest/NAR validation and gateway output registration;
- cancel/terminate only that execution and recover only that exact instance.

The existing worker has no reconnect after a dropped callback. Recovery must
address this explicitly; MicroVM memory persistence does not fix a broken socket.
The backend ADR covers durable identity, configuration, routing and authentication.
Product integration still requires the corresponding failure/restart contracts
and release gates; the historical fixtures do not satisfy them.

## Sources

- [Images, capabilities, sizing](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images.html)
- [Lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/microvms-launching.html)
- [Networking](https://docs.aws.amazon.com/lambda/latest/dg/microvms-networking.html)
- [Snapshots](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
- [Run API and idempotency](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_RunMicrovm.html)
- [Pricing](https://aws.amazon.com/lambda/pricing/)
