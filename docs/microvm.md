# Lambda MicroVM backend experiment

This is an experimental sandbox feasibility gate, **not an implemented Telchar
backend**. Disposable fixtures exercise authenticated WSS, NAR transfers and
validated output registration locally. No selectable gateway backend or execution
recovery is implemented. Production routing and persistence are unchanged. AWS Lambda MicroVMs are distinct from ordinary Lambda functions.

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
and runtime adapter for ARM packaging; its AWS WSS qualification is pending.
Receiver output buffering is bounded but remains memory-backed, so large-transfer
measurements must wait for disk staging. TLS snapshot uniqueness and reconnect
behavior remain unqualified.

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
A new backend touches durable identity, configuration, routing and authentication;
those changes need an ADR and failure/restart tests before product integration.

## Sources

- [Images, capabilities, sizing](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images.html)
- [Lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/microvms-launching.html)
- [Networking](https://docs.aws.amazon.com/lambda/latest/dg/microvms-networking.html)
- [Snapshots](https://docs.aws.amazon.com/lambda/latest/dg/microvms-images-snapshots.html)
- [Run API and idempotency](https://docs.aws.amazon.com/lambda/latest/microvm-api/API_RunMicrovm.html)
- [Pricing](https://aws.amazon.com/lambda/pricing/)
