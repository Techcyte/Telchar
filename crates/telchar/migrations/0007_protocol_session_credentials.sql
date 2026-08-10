ALTER TABLE protocol_sessions
    ADD COLUMN credential_id text,
    ADD COLUMN authentication_authority text;

ALTER TABLE protocol_sessions
    ADD CONSTRAINT protocol_sessions_credential_identity_check CHECK (
        (credential_id IS NULL AND authentication_authority IS NULL)
        OR (
            credential_id IS NOT NULL
            AND length(credential_id) BETWEEN 1 AND 1024
            AND authentication_authority IN ('openssh-public-key', 'openssh-certificate')
            AND (
                (authentication_authority = 'openssh-public-key' AND credential_id LIKE 'ssh-pubkey:_%')
                OR (authentication_authority = 'openssh-certificate' AND credential_id LIKE 'ssh-cert:_%')
            )
        )
    );
