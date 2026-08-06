# OpenSSH Public-Key Identity Handoff

**Status:** Proven for initial ingress

The forced command receives the authenticated public-key fingerprint through an OpenSSH-controlled `authorized_keys` command option. The fixture generates a key, computes its SHA256 fingerprint, places that fingerprint in the forced-command configuration, and connects through a real local `sshd` using public-key authentication.

The forced command records the configured authenticated fingerprint, the optional client-supplied identity environment value, and `SSH_ORIGINAL_COMMAND`. The fixture asserts:

- the authenticated fingerprint equals the key offered and accepted by OpenSSH;
- a client-provided `TELCHAR_CLIENT_SUPPLIED_KEY=spoofed` value cannot replace the authenticated fingerprint;
- the requested command is not treated as the identity source;
- PTY, agent forwarding, X11 forwarding, and TCP forwarding are disabled in the fixture configuration.

The production forced-command implementation must preserve this direction of trust: OpenSSH-controlled metadata enters the frontend; client-controlled environment variables, command arguments, and worker-protocol fields never select identity. The fingerprint is bounded and suitable as credential metadata; normalization and mapping to audit/quota subjects belong to T049 and later durable identity work.

Certificate metadata is not implied by this path and remains the separate T048 prototype.
