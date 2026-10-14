# Lambda MicroVM backend experiment

This is an experimental sandbox feasibility gate, **not an implemented Telchar
backend**. It does not submit gateway builds, authenticate worker callbacks,
transfer NARs, or recover executions. Production routing and persistence are
unchanged. AWS Lambda MicroVMs are distinct from ordinary Lambda functions.

## What exists

- `nix/microvm-probe.nix`: a Linux container with Nix, a root daemon, sixteen
  dedicated build users, and a lifecycle adapter. `sandbox = true` and
  `sandbox-fallback = false` are mandatory. No host store is mounted.
- `deploy/microvm/runtime.py`: bounded lifecycle hook requests. `/ready` and
  `/validate` check the daemon socket without executing a build before snapshot.
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

## AWS validation still required

Use an up-to-date AWS CLI/SDK exposing `lambda-microvms`; the development host's
AWS CLI 2.34.24 does not recognize that service. No resources are provisioned by
these tests.

Once approved for a test account:

1. Build the ARM image and push to an operator-owned ECR repository. Package a
   Dockerfile referencing its immutable digest into the S3 code artifact.
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
