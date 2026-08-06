use telchar::ipc::{IpcEnvelope, IpcError, RequesterMetadata, StreamAttachment, IPC_VERSION};

#[test]
fn envelope_round_trips_authenticated_metadata_and_attachment() {
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "ssh-pubkey:SHA256:fixture".into(),
            audit_subject: "builder".into(),
            quota_subject: "team-build".into(),
        },
        session_id: "session-001".into(),
        attachment: StreamAttachment { id: 42 },
        error: Some(IpcError {
            code: "protocol-rejected".into(),
            message: "unsupported operation".into(),
        }),
    };

    let encoded = envelope.encode().expect("envelope encodes");
    let decoded = IpcEnvelope::decode(&encoded).expect("envelope decodes");
    assert_eq!(decoded, envelope);
}

#[test]
fn envelope_rejects_unsupported_version_and_oversized_error() {
    let mut unsupported = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "credential".into(),
            audit_subject: "audit".into(),
            quota_subject: "quota".into(),
        },
        session_id: "session".into(),
        attachment: StreamAttachment { id: 1 },
        error: None,
    }
    .encode()
    .expect("envelope encodes");
    unsupported[4..6].copy_from_slice(&(IPC_VERSION + 1).to_le_bytes());
    assert!(IpcEnvelope::decode(&unsupported).is_err());

    let oversized = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "credential".into(),
            audit_subject: "audit".into(),
            quota_subject: "quota".into(),
        },
        session_id: "session".into(),
        attachment: StreamAttachment { id: 1 },
        error: Some(IpcError {
            code: "error".into(),
            message: "x".repeat(4097),
        }),
    };
    assert!(oversized.encode().is_err());
}
