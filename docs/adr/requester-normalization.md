# Requester Normalization

**Status:** Accepted for initial authenticated ingress

Requester normalization keeps authentication credential, audit attribution, quota ownership, certificate metadata, and source address distinct.

| Input | Credential ID | Audit subject | Quota subject | Certificate metadata | Source address |
| --- | --- | --- | --- | --- | --- |
| Public-key fingerprint `F`, no mappings | `ssh-pubkey:F` | `F` | credential ID | absent | optional OpenSSH transport address |
| Public-key fingerprint `F`, configured mappings | `ssh-pubkey:F` | configured audit subject | configured quota subject | absent | optional OpenSSH transport address |
| Certificate CA `C`, key ID `K`, principals `P...`, no mappings | length-prefixed `ssh-cert:<len(C)>:C:<len(K)>:K` | first principal | credential ID | CA, key ID, all principals | optional OpenSSH transport address |
| Certificate with configured mappings | length-prefixed certificate credential ID | configured audit subject | configured quota subject | CA, key ID, all principals | optional OpenSSH transport address |

All identity components and subjects are non-empty and bounded to 256 bytes. Empty certificate principal lists and empty components are rejected. Certificate credential IDs length-prefix the CA fingerprint and key ID so delimiter characters cannot create collisions. Source address is retained as context only; it never substitutes for credential, audit, or quota identity. Certificate metadata is represented only when authenticated certificate evidence exists; the T048 deferral means no certificate metadata is produced by the current public-key path.

Normalization is deterministic and does not inspect client-supplied environment variables, requested commands, worker-protocol payloads, or arbitrary SSH values. The implementation is in `crates/telchar/src/identity.rs`; table-driven tests cover public-key, mapped public-key, certificate, IPv4, IPv6, empty and oversized fields, exact size boundaries, and ambiguous delimiter inputs.
