//! Tests identity contracts and failure boundaries, including normalizes public key and certificate requesters deterministically.

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
            "ssh-pubkey:SHA256:abc",
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
            "ssh-cert:9:SHA256:ca:8:build-42",
            "builder",
            "ssh-cert:9:SHA256:ca:8:build-42",
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
fn certificate_credential_ids_are_unambiguous() {
    let first = normalize_requester(certificate_input("a:b", "c")).expect("identity normalizes");
    let second = normalize_requester(certificate_input("a", "b:c")).expect("identity normalizes");

    assert_ne!(first.credential_id, second.credential_id);
    assert_eq!(first.credential_id, "ssh-cert:3:a:b:1:c");
    assert_eq!(second.credential_id, "ssh-cert:1:a:3:b:c");
}

#[test]
fn accepts_identity_components_at_the_limit() {
    let value = "x".repeat(256);
    let public_key = normalize_requester(IdentityInput::PublicKey {
        fingerprint: value.clone(),
        audit_subject: Some(value.clone()),
        quota_subject: Some(value.clone()),
        source_address: None,
    });
    assert!(public_key.is_ok());

    let certificate = normalize_requester(IdentityInput::Certificate {
        ca_fingerprint: value.clone(),
        key_id: value.clone(),
        principals: vec![value.clone()],
        audit_subject: Some(value.clone()),
        quota_subject: Some(value),
        source_address: None,
    });
    assert!(certificate.is_ok());
}

#[test]
fn rejects_empty_authenticated_identity_components() {
    let cases = [
        (
            IdentityInput::PublicKey {
                fingerprint: String::new(),
                audit_subject: None,
                quota_subject: None,
                source_address: None,
            },
            "fingerprint",
        ),
        (certificate_input("", "key"), "CA fingerprint"),
        (certificate_input("ca", ""), "certificate key ID"),
        (
            IdentityInput::Certificate {
                ca_fingerprint: "ca".into(),
                key_id: "key".into(),
                principals: vec![String::new()],
                audit_subject: None,
                quota_subject: None,
                source_address: None,
            },
            "certificate principal",
        ),
        (
            IdentityInput::PublicKey {
                fingerprint: "key".into(),
                audit_subject: Some(String::new()),
                quota_subject: None,
                source_address: None,
            },
            "audit subject",
        ),
        (
            IdentityInput::PublicKey {
                fingerprint: "key".into(),
                audit_subject: None,
                quota_subject: Some(String::new()),
                source_address: None,
            },
            "quota subject",
        ),
    ];

    for (input, component) in cases {
        assert_eq!(
            normalize_requester(input),
            Err(telchar::identity::NormalizeError::EmptyComponent(component))
        );
    }

    let empty_principals = IdentityInput::Certificate {
        ca_fingerprint: "ca".into(),
        key_id: "key".into(),
        principals: Vec::new(),
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    };
    assert_eq!(
        normalize_requester(empty_principals),
        Err(telchar::identity::NormalizeError::EmptyPrincipal)
    );
}

#[test]
fn rejects_oversized_authenticated_identity_components() {
    let oversized = "x".repeat(257);
    let cases = [
        (
            IdentityInput::PublicKey {
                fingerprint: oversized.clone(),
                audit_subject: None,
                quota_subject: None,
                source_address: None,
            },
            "fingerprint",
        ),
        (certificate_input(&oversized, "key"), "CA fingerprint"),
        (certificate_input("ca", &oversized), "certificate key ID"),
        (
            IdentityInput::Certificate {
                ca_fingerprint: "ca".into(),
                key_id: "key".into(),
                principals: vec![oversized.clone()],
                audit_subject: None,
                quota_subject: None,
                source_address: None,
            },
            "certificate principal",
        ),
        (
            IdentityInput::PublicKey {
                fingerprint: "key".into(),
                audit_subject: Some(oversized.clone()),
                quota_subject: None,
                source_address: None,
            },
            "audit subject",
        ),
        (
            IdentityInput::PublicKey {
                fingerprint: "key".into(),
                audit_subject: None,
                quota_subject: Some(oversized),
                source_address: None,
            },
            "quota subject",
        ),
    ];

    for (input, component) in cases {
        assert_eq!(
            normalize_requester(input),
            Err(telchar::identity::NormalizeError::OversizedComponent(
                component
            ))
        );
    }
}

fn certificate_input(ca_fingerprint: &str, key_id: &str) -> IdentityInput {
    IdentityInput::Certificate {
        ca_fingerprint: ca_fingerprint.into(),
        key_id: key_id.into(),
        principals: vec!["builder".into()],
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    }
}
