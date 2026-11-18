# Client upload trust

Telchar can audit authenticated client uploads and optionally require trusted Nix signatures before accepting input-addressed paths. Both features default off. Verification applies only to client `AddMultipleToStore`, not executor output collection. Stock Nix worker messages are unchanged.

Signatures attest that a holder of a configured private key signed path metadata. They do not prove the bytes were built from the corresponding derivation. Developer host keys provide attribution; accepting only controlled builder, CI or upstream keys prevents unsigned developer-built input-addressed paths from entering through this upload operation. Trusting a shared cache key when developers can push arbitrary paths to that cache reopens that risk. All clients still share one trusted gateway store domain; path knowledge is not tenant authorization.

## Configuration

Enable audit independently for attribution. Verification does not require audit.

```toml
[audit]
enabled = true

[upload_verification]
enabled = true
min_signatures = 1
identity_match = "off" # off, warn, enforce
identity_field = "identity"
exempt_content_addressed = true
trusted_public_keys = [] # Nix name:base64 public keys

# [[upload_verification.trusted_keys]]
# public_key = "ci-1:BASE64_PUBLIC_KEY"
# valid_from = "2026-11-01T00:00:00Z"
# valid_until = "2027-06-01T00:00:00Z"
# revoked = false
# metadata = { identity = "ci", kind = "builder" }

# [upload_verification.resolver]
# file = "/var/lib/telchar/trusted-keys.json"
# command = ["/usr/local/bin/nix-upload-keys"]
# command_timeout = "10s"
# refresh_interval = "5m"
# unknown_key_refresh_limit = "6/min"
```

On NixOS, set these same attributes under `services.telchar.settings.upload_verification`. Public keys may appear in generated configuration; private keys and resolver credentials must not. Resolver programs acquire credentials through deployment-owned mechanisms, not client requests. Commands use an absolute executable and argv without a shell. Durations accept positive integer seconds, minutes or hours, up to one day.

The registry unions simple static keys, detailed static entries, a file and a command. Names derive from the public-key prefix. One simple record plus one detailed record for the same name and bytes uses the detailed validity/metadata. Two detailed records are ambiguous even if identical: static duplicates reject configuration, dynamic duplicates quarantine that name across sources. Different bytes sharing a name also conflict. Metadata values are flat strings, booleans or numbers, limited to 4096 serialized bytes per key. Per-upload evaluation retains at most 64 KiB of repeated signer metadata; non-exempt uploads exceeding that budget fail closed.

Resolver JSON uses camelCase:

```json
{"version":1,"keys":[{"publicKey":"ci-1:BASE64_PUBLIC_KEY","validFrom":null,"validUntil":null,"revoked":false,"metadata":{"identity":"ci","kind":"builder"}}]}
```

Replace the illustrative key with a real 32-byte Nix public key. Malformed entries are skipped. Failed reads/commands, unsupported versions and unexpected empty replacements retain that source's last-known-good snapshot. This retains prior trust, not newly granted trust; revoke with explicit revoked entries rather than returning an empty document. Revoked and expired entries remain available for audit but cannot authorize uploads. Runtime conflicts exclude the name rather than selecting a winner.

File changes are detected by modification time and size, with periodic refresh as a backstop. Unknown signature names can trigger an immediate rate-limited refresh; `0/min` disables that trigger. Refresh execution is serialized and command output/file input are bounded to 1 MiB. Slow resolver refresh can delay uploads until its configured command deadline. Resolver stderr content is withheld; diagnostics report its presence without copying potentially secret output.

## Signature decisions

Nix signs `1;<storePath>;<narHash>;<narSize>;<references>`, using `sha256:` plus Nix base32 for the hash, sorted full reference paths and commas between references. Each supplied signature is checked against registry key bytes. Labels do not establish signer identity. Thresholds count distinct public-key bytes; repeated signatures or aliases of the same key cannot satisfy a multiple-key requirement.

Signature statuses are `valid`, `unknown_key`, `invalid`, `expired`, `revoked` and `malformed`. Validity starts inclusively and ends exclusively; not-yet-valid keys are classified expired. Identity enforcement additionally needs at least one valid signer whose configured metadata field is a string equal to the authenticated session audit subject. Other valid identities can contribute to the threshold. `warn` records mismatches without rejecting; `off` skips identity matching.

Already-valid reuploads undergo the same policy checks. Valid signatures never bypass NAR hash/size validation. Content-addressed exemption leaves Nix daemon validation mandatory: a claimed content address alone is not evidence that the path and bytes match it. It is not an exemption from content validation.

## Client signing

Configure protected `secret-key-files` in the local Nix daemon's `nix.conf` to sign locally produced paths. Existing paths can be signed explicitly:

```sh
nix store sign -k /run/secrets/nix-signing-key --all
```

Signatures must already be registered in the source store before copying to Telchar. Signatures created only by an Attic push-time signer do not automatically appear in a stock Nix SSH upload. Never place private keys in world-readable Nix-generated configuration.

Rejecting an unsigned input does not itself request a rebuild. Telchar adds no automatic retries or scheduler fallback. Use an appropriately signed source store, or build dependencies in a trusted builder workflow; do not assume rejecting a local path makes the stock client rebuild it remotely.

## Deny-only external policy

With verification enabled, an optional hook makes a final site-specific decision for each non-exempt path, including already-valid reuploads. Signature/identity rejection never invokes it, and a hook cannot authorize an otherwise rejected upload. Content-addressed exemptions skip the hook. Absent hook means no external decision.

```toml
[upload_verification.policy_hook]
url = "http://127.0.0.1:9090/decide"
# Alternatively: command = ["/usr/local/bin/nix-upload-policy"]
timeout = "2s"
```

Configure exactly one command or HTTP(S) URL. A command receives JSON on stdin without a shell. HTTP POST does not follow redirects. Responses are limited to 64 KiB; timeouts, nonzero exits, invalid JSON, missing boolean `allow` and non-2xx status fail closed. Blocked pipes share the execution deadline. Command stderr is discarded, not copied into logs.

Request shape:

```json
{"version":1,"upload":{"storePath":"/nix/store/…","narHash":"sha256:HEX","narSize":1234,"references":[],"contentAddress":null,"alreadyValid":false},"token":{"sub":"alice"},"signatures":[{"keyName":"host-1","status":"valid","source":"file","metadata":{"identity":"alice","kind":"host"}}]}
```

`token.sub` is the authenticated Telchar audit subject, not a client claim. Hashes in this JSON use hex; the cryptographic Nix fingerprint uses base32. Unknown keys have null source/metadata. `alreadyValid` is null if the store lookup failed. Responses are `{"allow":true}` or `{"allow":false,"reason":"site requirement"}`. Reasons are limited to 4096 bytes without control characters and reach client/audit output: never return credentials or secrets. Use URL form for high volume; command form launches one process per path, without batching.

Telchar carries `contentAddress` and `alreadyValid`; Attic's hook instead carries cache name and `deduplicated`. Policies using those product-specific fields need corresponding logic. Shared signer metadata and response semantics do not imply byte-identical request payloads.

## Related implementation contract

Registry JSON and duplicate handling were checked against the Attic fork feature source at `5e76942d356b875c99aafdf9d07710790612e3c9`, not upstream/main. Telchar uses its normal snake_case TOML naming, whereas that fork uses kebab-case. This is format provenance, not a claim that either product's entire configuration or policy request can be used unchanged in the other.
