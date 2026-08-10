# Terminal failure classification

## Status

Accepted for the Gate 4 execution lifecycle.

## Decision

Telchar persists terminal execution failures using this closed classification set:

- `build-failure`: the derivation executed and reported failure.
- `infrastructure-failure`: allocation, machine, network, executor, or backend transport failed.
- `admission-failure`: quota or policy rejected accepted work before execution.
- `input-failure`: required derivation or input paths could not be staged or verified.
- `output-failure`: produced outputs could not be collected or verified.
- `cancelled`: Telchar authoritatively completed cancellation.
- `internal-failure`: Telchar could not safely continue because of an internal gateway failure.

The classification is immutable terminal history. It describes the observed failure domain. It does not itself authorize retry, reconciliation, cancellation, or resubmission.

Backend states such as `absent`, `pending`, `running`, `completed`, `failed`, `cancelled`, and `ambiguous` are observations, not terminal failure classifications. An ambiguous or possibly active attempt must remain nonterminal until recovery establishes an authoritative result.

## Retry boundary

Retry and reconciliation policy remain owned by the later failure transition matrix. In particular:

- `build-failure` does not imply retry.
- `infrastructure-failure` does not imply retry.
- no classification creates another attempt by itself.
- recovery must establish that the previous attempt is inactive or fenced before any retry decision.

This separation keeps persisted history stable while allowing retry policy to remain state- and evidence-dependent.

## Persistence contract

A terminal failure transaction must:

1. lock the active attempt and its request;
2. validate that the classification belongs to the closed set;
3. preserve bounded structured result metadata;
4. release any active capacity reservation owned by the attempt;
5. set the attempt to `failed` with a monotonic completion timestamp;
6. set the request to `failed`;
7. create exactly one immutable execution outcome; and
8. commit all terminal records together.

Missing classification, unsupported classification, malformed metadata, terminal replacement, mismatched request state, or a missing active reservation fails closed without a partial transition.

## Non-goals

This decision does not define:

- retry eligibility;
- retry attempt limits;
- failure-point-to-classification mapping for every backend;
- precedence for cancellation/completion races;
- reconciliation behavior for ambiguous backend activity; or
- Nix `BuildResult` mapping.

Those remain separate dependency-ordered tasks.
