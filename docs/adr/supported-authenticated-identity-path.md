# Supported Authenticated Identity Path

**Status:** Approved for initial ingress

## Approved mechanism

Initial ingress supports one OpenSSH-controlled identity path: public-key authentication with the fingerprint supplied by the `authorized_keys` forced-command configuration. The OpenSSH ingress integration tests prove the key fingerprint observed by the forced command matches the key accepted by OpenSSH and that a client-supplied identity value cannot replace it.

The frontend may carry this bounded credential ID to the daemon through the authenticated local IPC envelope defined by T050/T051. T049 defines deterministic requester normalization. The source address remains audit context and never becomes credential or quota identity.

## Deferred mechanisms

- OpenSSH user certificates: deferred by T048 until a real fixture captures CA fingerprint, certificate key ID, and principals securely.
- Client environment variables, requested commands, and worker-protocol fields: rejected as identity sources.
- Source address alone: rejected as an authenticated identity source.

## Gate decision

At least one spoof-resistant OpenSSH-controlled path is proven, so Gate 2 is not blocked by the identity prototype. Certificate support remains explicitly deferred and cannot be silently assumed by later ingress code.

## Verification checklist

- [x] Approved path is controlled by OpenSSH.
- [x] Real public-key authentication fixture proves accepted fingerprint.
- [x] Negative spoof test proves client metadata cannot replace identity.
- [x] Certificate path is explicitly deferred with evidence requirements.
- [x] Source address is classified as context, not identity.
- [x] Later IPC and normalization tasks are named rather than pre-implemented here.
