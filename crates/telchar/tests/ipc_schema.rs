//! Tests ipc schema contracts and failure boundaries, including envelope round trips authenticated metadata and session.

use telchar::service::identity::{IdentityInput, normalize_requester};
use telchar::service::ipc::{IPC_VERSION, IpcEnvelope, IpcError, RequesterMetadata};
use telchar_telemetry::TraceContext;

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
        trace_context: TraceContext::new(
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()),
            Some("vendor=value".into()),
        )
        .expect("trace context validates"),
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
        trace_context: telchar_telemetry::TraceContext::default(),
        error: None,
    };

    let encoded = envelope.encode().expect("maximum requester encodes");
    assert_eq!(IpcEnvelope::decode(&encoded).unwrap(), envelope);
}

#[test]
fn daemon_decodes_version_one_envelopes_without_trace_context() {
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata {
            credential_id: "credential".into(),
            audit_subject: "audit".into(),
            quota_subject: "quota".into(),
        },
        session_id: "session".into(),
        trace_context: TraceContext::default(),
        error: None,
    };
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"TIPC");
    encoded.extend_from_slice(&1_u16.to_le_bytes());
    for value in [
        envelope.requester.credential_id.as_str(),
        envelope.requester.audit_subject.as_str(),
        envelope.requester.quota_subject.as_str(),
        envelope.session_id.as_str(),
    ] {
        encoded.extend_from_slice(&(value.len() as u16).to_le_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    encoded.push(0);

    let decoded = IpcEnvelope::decode(&encoded).expect("version one envelope decodes");
    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.requester, envelope.requester);
    assert_eq!(decoded.session_id, envelope.session_id);
    assert_eq!(decoded.trace_context, TraceContext::default());
    assert_eq!(decoded.error, envelope.error);
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
        trace_context: telchar_telemetry::TraceContext::default(),
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
        trace_context: telchar_telemetry::TraceContext::default(),
        error: Some(IpcError {
            code: "error".into(),
            message: "x".repeat(4097),
        }),
    };
    assert!(oversized.encode().is_err());
}
