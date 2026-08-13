# OpenSSH Certificate Identity Handoff

**Status:** Deferred

The initial ingress identity path is the OpenSSH-controlled public-key fingerprint proven by the OpenSSH ingress integration tests. Certificate support is not approved for initial ingress because this packet has not yet captured a real OpenSSH certificate authentication fixture that securely records the signing CA fingerprint, certificate key ID, and principals at the forced-command boundary.

No certificate metadata is inferred from a public-key fingerprint. No client-supplied environment variable, command argument, or worker-protocol field may stand in for the CA, key ID, or principals. T048 is therefore satisfied by explicit deferral, not by pretending certificate identity is supported.

Certificate support requires a separate real-sshd fixture that:

- creates a CA and signed user certificate;
- authenticates through OpenSSH with `TrustedUserCAKeys`;
- captures OpenSSH-controlled CA fingerprint, key ID, and principals;
- proves spoofed client metadata cannot replace any captured value;
- records bounded normalization inputs and negative cases;
- adds telemetry assertions without exporting raw certificate contents as metric attributes.

Until that evidence exists, certificate metadata remains absent from the supported requester identity and certificate-aware normalization is deferred.
