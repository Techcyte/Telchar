//! Tests ipc schema contracts and failure boundaries, including envelope round trips authenticated metadata and session.

use telchar::service::identity::{normalize_requester, IdentityInput};
use telchar::service::ipc::{IpcEnvelope, IpcError, RequesterMetadata, IPC_VERSION};

#[test]
fn envelope_round_trips_authenticated_metadata_and_session() {
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "ssh-pubkey:SHA256:fixture".into(),
            audit_subject: "builder".into(),
            quota_subject: "team-build".into(),
        },
        session_id: "session-001".into(),
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
fn maximum_normalized_requester_fits_the_ipc_envelope() {
    let requester = normalize_requester(IdentityInput::Certificate {
        ca_fingerprint: "c".repeat(256),
        key_id: "k".repeat(256),
        principals: vec!["p".repeat(256)],
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    })
    .expect("maximum requester normalizes");
    let metadata = RequesterMetadata::try_from(&requester).expect("maximum requester converts");
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: metadata,
        session_id: "session".into(),
        error: None,
    };

    let encoded = envelope.encode().expect("maximum requester encodes");
    assert_eq!(IpcEnvelope::decode(&encoded).unwrap(), envelope);
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
        error: Some(IpcError {
            code: "error".into(),
            message: "x".repeat(4097),
        }),
    };
    assert!(oversized.encode().is_err());
}
