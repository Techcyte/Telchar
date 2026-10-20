# Native packaged runtime exchange

`runtime-exchange.py` exercises a prepared ARM64 AWS root with its packaged Nix and dedicated worker. The gateway fixture uses real PostgreSQL, authenticated Unix IPC, TLS/WebSocket transport and isolated canonical stores; provider HTTP responses are simulated. This is not OpenSSH ingress, live AWS, OCI-builder equivalence or restored-snapshot qualification.

## Prerequisites

- Native ARM64 Linux, subordinate user/group mappings covering image build users, private mount/PID namespaces and overlayfs.
- Prepared root matching `deploy/microvm/Dockerfile`, including standard device mount points and `/dev/ptmx -> pts/ptmx`.
- Built `microvm-aws-context` with the repository test certificate, available unchanged worker/Nix closure and packaged Bash.
- Compiled `microvm_build` integration test and check-only daemon with the loopback provider seam. Never install that daemon into the image.
- Gateway command with the realized development environment, working directory matching the compiled test source, and `TELCHAR_TEST_DAEMON_BIN` set. The command runs as namespace UID1000/GID100 and must preserve the harness's `MICROVM_*` environment variables.

Run only inside a disposable private mount/PID namespace with mapped build users. The harness mounts filesystems at paths inside the supplied root. It preserves its evidence directory and overlay writes; it does not delete the prepared root. Worker store, database, temporary files and binding state are isolated per invocation. Gateway fixture separately isolates its store.

Within the recorded, verified resource envelope (8GiB memory, no swap,256tasks,200%CPU, `CARGO_BUILD_JOBS=1`, `PYTHONDONTWRITEBYTECODE=1`):

```sh
python3 tests/microvm/runtime-exchange.py \
  --root "$PREPARED_ROOT" \
  --context "$AWS_CONTEXT" \
  --evidence "$EVIDENCE_DIRECTORY" \
  --builder "$PACKAGED_BASH/bin/bash" \
  bash "$GATEWAY_COMMAND"
```

The script must be executable: the Rust fixture invokes it with a binding-file argument to bridge the generated provider payload into the runtime run hook. Port8080 must be free within the test network namespace.

Success requires the worker to request the fixture input and emit its sandbox-builder execution marker, then return the exact expected content through gateway validation/registration. The builder asserts nonroot execution and absence of protected binding/runtime paths. Existing daemon assertions require durable successful MicroVM identity, consumed callback session, detached request, retained provider reservation, replay rejection and clean shutdown. The harness additionally requires successful runtime status and empty captured gateway/runtime error streams.

Runtime-mode fixture limits are64MiB per NAR and128MiB total for the packaged Bash closure; default shell-fixture limits remain1MiB/2MiB. Missing prerequisites or policy failures must not be addressed by disabling production sandboxing.
