# Telchar

Telchar is a self-hosted gateway for distributing Nix builds across execution backends while preserving the stock Nix client and worker protocol.

Nix remains responsible for evaluation and deciding which derivations are ready to build. Telchar aims to provide:

- One stable remote-builder endpoint for stock Nix clients.
- Central admission control, quotas, fair queueing, and scheduling.
- Backend selection across local Nix, static SSH builders, Nomad jobs, and future providers.
- Durable request and execution-attempt tracking in PostgreSQL.
- Input staging, output collection, cancellation, retry classification, and restart recovery.
- Correlated OpenTelemetry logs, metrics, and traces.
- Optional binary-cache integration that never affects build correctness.

Initial deployments use one active Telchar daemon. Restricted OpenSSH forced-command frontends carry worker-protocol sessions to the daemon over authenticated local IPC. The daemon owns scheduling, durable state, gateway-store coordination, and backend reconciliation.

Authenticated clients initially share one mutually trusted Nix store domain. Telchar does not initially provide hostile multi-tenant isolation or per-path authorization.

See the [design brief](telchar-design.md), [implementation plan](TELCHAR_IMPLEMENTATION_PLAN.md), and [deployment assumptions](docs/adr/deployment-assumptions.md) for the complete architecture and delivery plan.

## License

Telchar is licensed under the [MIT License](LICENSE).
