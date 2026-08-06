# NixOS Test Topology

**Status:** Accepted

## Context

Telchar crosses service, network, Nix-client, SSH, telemetry, persistence, builder, and cache boundaries. Unit tests and process-local fixtures cannot establish that packaged services own those boundaries correctly. The project needs one reusable whole-system acceptance harness before protocol behavior grows around incompatible test arrangements.

## Decision

The authoritative whole-system acceptance harness is a flake-exported multi-machine `nixosTest`.

The baseline topology is:

```text
stock-client -- virtual network --> gateway -- OTLP/gRPC --> otlp-collector
```

- `stock-client` is an unmodified NixOS Nix client. It starts independently on the virtual network.
- `gateway` runs the packaged Telchar systemd oneshot service and owns its gateway-store configuration.
- `otlp-collector` is a real or protocol-compatible OTLP gRPC collector. It stores bounded test records for correlated logs, metrics, and traces.

The virtual-network topology is asserted independently from Telchar protocol reachability. The packaged `telchar` systemd oneshot service owns startup. Test-driver commands do not start the binary directly. Baseline readiness requires the packaged Telchar systemd oneshot service to complete successfully and correlated OTLP startup telemetry to reach the collector. Real client-to-Telchar protocol reachability remains assigned to Gate 2 OpenSSH integration tasks.

The shared NixOS test library exports modules and helpers for Telchar packaging, stock-Nix client configuration, virtual networking, OpenSSH, OTLP collection, startup and readiness, cleanup, and artifacts. Tests compose those helpers rather than duplicating machine setup.

Failure capture retains bounded, redacted service journals, machine state, OTLP records, and test-driver output. Capture limits are part of the helper contract. Successful runs remove temporary capture state and emit no diagnostic output. Secrets enter only through NixOS test secret facilities or runtime files with mode `0600`. Artifact capture redacts secrets before retention.

PostgreSQL, OpenSSH builder, Nomad, and cache fixtures extend this topology through the shared helpers; they do not create a second orchestration harness. Each extension declares its machine role, readiness rule, network boundaries, and bounded artifact policy.

## Extension map

| Future task area | Extension | Reused baseline contract |
| --- | --- | --- |
| T021C baseline smoke | gateway service and collector | all baseline machines, readiness, network, telemetry |
| T021D failure artifacts | controlled gateway failure | capture, redaction, cleanup |
| T021E repository gate | flake check target | direct smoke target and aggregate check |
| PostgreSQL execution | PostgreSQL machine or service | gateway readiness, network, artifacts |
| restricted OpenSSH ingress | SSH configuration on gateway | stock client, OpenSSH, network, capture |
| SSH builders and Nomad | builder or Nomad machines | topology composition, readiness, capture |
| cache integration | cache machine or service | network, telemetry, cleanup |

## Consequences

A test that crosses an external Telchar boundary extends the shared `nixosTest` harness. Narrow library tests remain appropriate for local behavior, but cannot replace whole-system acceptance. A harness boot, network ping, service oneshot, or nonempty telemetry file proves only that specific behavior; it is not authoritative evidence for protocol, ingress, correlation, persistence, or execution until the composed production path is exercised and asserted. This makes machine roles, service ownership, startup sequencing, telemetry, and failure diagnostics explicit and reusable.
