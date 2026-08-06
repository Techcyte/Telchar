# Initial worker-operation allowlist

## Scope

This allowlist applies only to the two accepted fixed classic input-addressed
compatibility fixtures at pinned Nix 2.34.7 and negotiated worker version 1.38:
`trusted-classic-build-v1` and `untrusted-classic-build-v1`. It classifies the
operation codes in their sanitized typed trace artifacts under
`docs/compatibility-traces/`. It is not a generic worker-protocol or service
allowlist.

Every admitted operation has the exact typed boundary, version gate, finite
fixture limit, and fail-closed behavior in
`docs/protocol-fixture-flow-inventory.md`. The transparent observer accepts no
request body based only on a byte chunk.

## Required operations

These operations occurred in both accepted traces and are required to replay
the fixed fixture:

| Operation | Code | Typed fixture role |
| --- | --- | --- |
| `SetOptions` (`19`) | Client option request |
| `IsValidPath` (`1`) | Store-path validity query |
| `AddToStore` (`7`) | Bounded framed classic staging upload |
| `QueryMissing` (`40`) | Missing-path query |
| `QueryPathInfo` (`26`) | Typed path-info query |
| `BuildPathsWithResults` (`46`) | Fixed classic build request and result |

## Optional operations

| Operation | Behavior |
| --- | --- |
| `AddTempRoot` (`11`) | Typed and accepted when the fixed client sends it before the required sequence; absent only where the client does not request a temporary root. |

## Recognized and rejected operations

`AddToStoreNar` and `AddMultipleToStore` are recognized as upload-operation
classes by the pinned worker protocol, but neither belongs to the accepted
classic fixture inventory. The observer rejects them before relaying any
untyped body. Content-addressed build-specific operations are likewise
recognized as unsupported for MVP and rejected before their body is read.

## Unknown operations

Every code outside the required and optional rows is unknown for this fixture
allowlist. The observer fails closed before forwarding an untyped request,
response, callback, or upload path. A future admission needs a concrete
fixture, primary serializer evidence, typed parser boundaries, finite limits,
negative tests, and a new sanitized trace classification.

## Classification result

`sh scripts/check-worker-operation-allowlist.sh` validates the two bounded
trace artifacts. It reports zero unclassified operations: `1`, `7`, `11`,
`19`, `26`, `40`, and `46` are all classified above.
