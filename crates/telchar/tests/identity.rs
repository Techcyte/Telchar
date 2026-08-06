use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use telchar::identity::{CertificateIdentity, IdentityInput, normalize_requester};

#[test]
fn normalizes_public_key_and_certificate_requesters_deterministically() {
    let cases = [
        (
            IdentityInput::PublicKey {
                fingerprint: "SHA256:abc".into(),
                audit_subject: None,
                quota_subject: None,
                source_address: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            },
            "ssh-pubkey:SHA256:abc",
            "SHA256:abc",
            "SHA256:abc",
            None,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ),
        (
            IdentityInput::PublicKey {
                fingerprint: "SHA256:def".into(),
                audit_subject: Some("alice".into()),
                quota_subject: Some("team-a".into()),
                source_address: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            },
            "ssh-pubkey:SHA256:def",
            "alice",
            "team-a",
            None,
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ),
        (
            IdentityInput::Certificate {
                ca_fingerprint: "SHA256:ca".into(),
                key_id: "build-42".into(),
                principals: vec!["builder".into(), "ops".into()],
                audit_subject: None,
                quota_subject: None,
                source_address: None,
            },
            "ssh-cert:SHA256:ca:build-42",
            "builder",
            "ssh-cert:SHA256:ca:build-42",
            Some(CertificateIdentity {
                ca_fingerprint: "SHA256:ca".into(),
                key_id: "build-42".into(),
                principals: vec!["builder".into(), "ops".into()],
            }),
            None,
        ),
    ];

    for (input, credential_id, audit_subject, quota_subject, certificate, source_address) in cases {
        let requester = normalize_requester(input).expect("identity normalizes");
        assert_eq!(requester.credential_id, credential_id);
        assert_eq!(requester.audit_subject, audit_subject);
        assert_eq!(requester.quota_subject, quota_subject);
        assert_eq!(requester.certificate, certificate);
        assert_eq!(requester.source_address, source_address);
    }
}

#[test]
fn rejects_missing_or_oversized_authenticated_identity_components() {
    let empty = normalize_requester(IdentityInput::PublicKey {
        fingerprint: String::new(),
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    });
    assert!(matches!(
        empty,
        Err(telchar::identity::NormalizeError::EmptyComponent(
            "fingerprint"
        ))
    ));

    let oversized = normalize_requester(IdentityInput::Certificate {
        ca_fingerprint: "ca".into(),
        key_id: "k".into(),
        principals: vec!["p".repeat(257)],
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    });
    assert!(matches!(
        oversized,
        Err(telchar::identity::NormalizeError::OversizedComponent(
            "certificate principal"
        ))
    ));
}
