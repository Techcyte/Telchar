use super::*;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use nix_worker_protocol::{
    ProtocolSessionLimits, WorkerReader, WorkerVersion, write_worker_byte_string,
    write_worker_integer,
};

const PATH: &str = "/nix/store/00000000000000000000000000000000-upload";
const REF_A: &str = "/nix/store/00000000000000000000000000000000-a";
const REF_Z: &str = "/nix/store/00000000000000000000000000000000-z";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
// Independently fixed Nix base32 representation of HASH, not calculated by the signer.
const NIX_HASH: &str = "1vydmf4nfi9307pwvaw9cx2j60ggrnmqjrs54c0yzkdbi5kla8q1";

struct Upload {
    path: String,
    hash: String,
    size: u64,
    references: Vec<String>,
    signatures: Vec<Vec<u8>>,
    content_address: Option<String>,
}

impl Default for Upload {
    fn default() -> Self {
        Self {
            path: PATH.into(),
            hash: HASH.into(),
            size: 123,
            references: vec![REF_Z.into(), REF_A.into()],
            signatures: Vec::new(),
            content_address: None,
        }
    }
}

impl Upload {
    // Exercise the production bounded protocol parser rather than constructing an unbounded model.
    fn with_info<T>(&self, mut test: impl FnMut(&AddMultipleToStorePathInfo) -> T) -> T {
        let mut body = Vec::new();
        write_worker_integer(&mut body, 1);
        write_worker_byte_string(&mut body, self.path.as_bytes());
        write_worker_byte_string(&mut body, b"");
        write_worker_byte_string(&mut body, self.hash.as_bytes());
        write_worker_integer(&mut body, self.references.len() as u64);
        for reference in &self.references {
            write_worker_byte_string(&mut body, reference.as_bytes());
        }
        write_worker_integer(&mut body, 0);
        write_worker_integer(&mut body, self.size);
        write_worker_integer(&mut body, 0);
        write_worker_integer(&mut body, self.signatures.len() as u64);
        for signature in &self.signatures {
            write_worker_byte_string(&mut body, signature);
        }
        write_worker_byte_string(
            &mut body,
            self.content_address
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        body.extend(vec![0; self.size as usize]);
        let mut wire = Vec::new();
        write_worker_integer(&mut wire, 0);
        write_worker_integer(&mut wire, 1);
        write_worker_integer(&mut wire, body.len() as u64);
        wire.extend(body);
        write_worker_integer(&mut wire, 0);
        let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
        let mut result = None;
        reader
            .complete_add_multiple_to_store(WorkerVersion::new(1, 38), |info, nar| {
                result = Some(test(info));
                std::io::copy(nar, &mut std::io::sink())?;
                Ok(())
            })
            .unwrap();
        result.unwrap()
    }

    fn evaluate(
        &self,
        config: &UploadVerification,
        keys: &BTreeMap<String, RegisteredKey>,
    ) -> Evaluation {
        self.with_info(|info| evaluate(info, keys, config, "alice", now()))
    }
}

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
}

fn config() -> UploadVerification {
    UploadVerification {
        enabled: true,
        ..UploadVerification::default()
    }
}

fn key_pair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn registered(name: &str, pair: &Ed25519KeyPair) -> RegisteredKey {
    RegisteredKey {
        name: name.into(),
        bytes: pair.public_key().as_ref().try_into().unwrap(),
        source: "static",
        detailed: true,
        valid_from: None,
        valid_until: None,
        revoked: false,
        metadata: BTreeMap::from([("identity".into(), MetadataValue::String("alice".into()))]),
    }
}

fn keys(entries: Vec<RegisteredKey>) -> BTreeMap<String, RegisteredKey> {
    entries
        .into_iter()
        .map(|key| (key.name.clone(), key))
        .collect()
}

fn fingerprint_bytes() -> Vec<u8> {
    format!("1;{PATH};sha256:{NIX_HASH};123;{REF_A},{REF_Z}").into_bytes()
}

fn sign(name: &str, pair: &Ed25519KeyPair) -> Vec<u8> {
    format!(
        "{name}:{}",
        base64::engine::general_purpose::STANDARD.encode(pair.sign(&fingerprint_bytes()).as_ref())
    )
    .into_bytes()
}

#[test]
fn repeated_malformed_signatures_have_bounded_metadata_reports() {
    let pair = key_pair(7);
    let mut key = registered("ci", &pair);
    key.metadata
        .insert("identity".into(), MetadataValue::String("x".repeat(4000)));
    let keys = keys(vec![key]);
    let upload = Upload {
        signatures: vec![b"ci:AAAA".to_vec(); 256],
        ..Upload::default()
    };
    let result = upload.evaluate(&config(), &keys);
    assert!(!result.allowed);
    assert_eq!(result.reason, EvaluationReason::MetadataLimit);
    assert!(serde_json::to_vec(&result).unwrap().len() < 128 * 1024);
}

#[test]
fn fingerprint_is_exact_nix_format_with_sorted_set_references() {
    let upload = Upload::default();
    upload.with_info(|info| assert_eq!(fingerprint(info).unwrap(), fingerprint_bytes()));
    let upload = Upload {
        references: vec![REF_Z.into(), REF_A.into(), REF_A.into()],
        ..Upload::default()
    };
    upload.with_info(|info| assert_eq!(fingerprint(info).unwrap(), fingerprint_bytes()));
    let upload = Upload {
        references: Vec::new(),
        ..Upload::default()
    };
    upload.with_info(|info| {
        assert_eq!(
            fingerprint(info).unwrap(),
            format!("1;{PATH};sha256:{NIX_HASH};123;").as_bytes()
        );
    });
}

#[test]
fn verifies_signature_and_every_fingerprint_component_is_bound() {
    let pair = key_pair(7);
    let keys = keys(vec![registered("ci", &pair)]);
    let mut upload = Upload {
        signatures: vec![sign("ci", &pair)],
        ..Upload::default()
    };
    let result = upload.evaluate(&config(), &keys);
    assert!(result.allowed);
    assert_eq!(result.reason, EvaluationReason::SignaturesVerified);
    assert_eq!(result.valid_signatures, 1);
    assert_eq!(result.signatures[0].status, SignatureStatus::Valid);
    upload.path = REF_A.into();
    assert_eq!(
        upload.evaluate(&config(), &keys).signatures[0].status,
        SignatureStatus::Invalid
    );
    upload.path = PATH.into();
    upload.hash = "ff".repeat(32);
    assert!(!upload.evaluate(&config(), &keys).allowed);
    upload.hash = HASH.into();
    upload.size += 1;
    assert!(!upload.evaluate(&config(), &keys).allowed);
    upload.size -= 1;
    upload.references.pop();
    assert!(!upload.evaluate(&config(), &keys).allowed);
}

#[test]
fn threshold_counts_distinct_key_bytes_not_names_or_repeated_signatures() {
    let first = key_pair(7);
    let second = key_pair(8);
    let keys = keys(vec![
        registered("first", &first),
        registered("alias", &first),
        registered("second", &second),
    ]);
    let config = UploadVerification {
        min_signatures: Some(2),
        ..config()
    };
    let mut upload = Upload {
        signatures: vec![
            sign("first", &first),
            sign("alias", &first),
            sign("first", &first),
        ],
        ..Upload::default()
    };
    let result = upload.evaluate(&config, &keys);
    assert!(!result.allowed);
    assert_eq!(result.valid_signatures, 1);
    assert_eq!(result.reason, EvaluationReason::InsufficientSignatures);
    upload.signatures.push(sign("second", &second));
    assert!(upload.evaluate(&config, &keys).allowed);
}

#[test]
fn status_precedence_is_malformed_then_revoked_then_expired_then_crypto() {
    let pair = key_pair(7);
    let other = key_pair(8);
    let mut key = registered("ci", &pair);
    key.revoked = true;
    key.valid_until = Some(now());
    let mut upload = Upload {
        signatures: vec![b"ci:not-base64".to_vec()],
        ..Upload::default()
    };
    assert_eq!(
        upload
            .evaluate(&config(), &keys(vec![key.clone()]))
            .signatures[0]
            .status,
        SignatureStatus::Malformed
    );
    upload.signatures = vec![sign("ci", &other)];
    assert_eq!(
        upload
            .evaluate(&config(), &keys(vec![key.clone()]))
            .signatures[0]
            .status,
        SignatureStatus::Revoked
    );
    key.revoked = false;
    assert_eq!(
        upload
            .evaluate(&config(), &keys(vec![key.clone()]))
            .signatures[0]
            .status,
        SignatureStatus::Expired
    );
    key.valid_until = None;
    assert_eq!(
        upload.evaluate(&config(), &keys(vec![key])).signatures[0].status,
        SignatureStatus::Invalid
    );
}

#[test]
fn validity_is_inclusive_from_and_exclusive_until() {
    let pair = key_pair(7);
    let upload = Upload {
        signatures: vec![sign("ci", &pair)],
        ..Upload::default()
    };
    let mut key = registered("ci", &pair);
    key.valid_from = Some(now());
    key.valid_until = Some(now() + time::Duration::seconds(1));
    assert!(upload.evaluate(&config(), &keys(vec![key.clone()])).allowed);
    key.valid_from = Some(now() + time::Duration::seconds(1));
    assert_eq!(
        upload
            .evaluate(&config(), &keys(vec![key.clone()]))
            .signatures[0]
            .status,
        SignatureStatus::Expired
    );
    key.valid_from = None;
    key.valid_until = Some(now());
    assert_eq!(
        upload.evaluate(&config(), &keys(vec![key])).signatures[0].status,
        SignatureStatus::Expired
    );
}

#[test]
fn malformed_and_unknown_signatures_do_not_leak_raw_bytes_or_invent_metadata() {
    let pair = key_pair(7);
    let keys = keys(vec![registered("ci", &pair)]);
    let mut signatures = vec![
        sign("unknown", &pair),
        b"no-colon-secret".to_vec(),
        b":AAAA".to_vec(),
        b"bad\nname:AAAA".to_vec(),
        vec![0xff, b':', b'A'],
        b"ci:AAAA".to_vec(),
        format!("ci:{}", "A".repeat(88)).into_bytes(),
        format!("ci:{}", "!".repeat(88)).into_bytes(),
        vec![b'A'; nix_worker_protocol::MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURE_BYTES],
    ];
    signatures.push(
        format!(
            "ci:{}",
            base64::engine::general_purpose::STANDARD.encode([0u8; 65])
        )
        .into_bytes(),
    );
    let upload = Upload {
        signatures,
        ..Upload::default()
    };
    let result = upload.evaluate(&config(), &keys);
    assert!(!result.allowed);
    assert_eq!(result.signatures[0].status, SignatureStatus::UnknownKey);
    assert_eq!(result.signatures[0].source, None);
    assert_eq!(result.signatures[0].metadata, None);
    assert!(
        result.signatures[1..]
            .iter()
            .all(|signature| signature.status == SignatureStatus::Malformed)
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(
        json["signatures"][0],
        serde_json::json!({"keyName":"unknown", "status":"unknown_key", "source":null, "metadata":null})
    );
    assert_eq!(json["signatures"][1]["keyName"], serde_json::Value::Null);
    assert_eq!(json["signatures"][5]["source"], "static");
    assert_eq!(json["signatures"][5]["metadata"]["identity"], "alice");
    assert!(!json.to_string().contains("no-colon-secret"));
}

#[test]
fn identity_off_warn_enforce_and_custom_field_use_only_valid_registry_metadata() {
    let pair = key_pair(7);
    let upload = Upload {
        signatures: vec![sign("ci", &pair)],
        ..Upload::default()
    };
    let mut key = registered("ci", &pair);
    key.metadata
        .insert("identity".into(), MetadataValue::String("bob".into()));
    let keys = keys(vec![key]);
    let off = upload.evaluate(&config(), &keys);
    assert!(off.allowed);
    assert!(!off.identity_mismatch);
    let warn = UploadVerification {
        identity_match: IdentityMatch::Warn,
        ..config()
    };
    let result = upload.evaluate(&warn, &keys);
    assert!(result.allowed);
    assert!(result.identity_mismatch);
    let enforce = UploadVerification {
        identity_match: IdentityMatch::Enforce,
        ..config()
    };
    let result = upload.evaluate(&enforce, &keys);
    assert!(!result.allowed);
    assert_eq!(result.reason, EvaluationReason::IdentityMismatch);
    for metadata in [
        BTreeMap::new(),
        BTreeMap::from([("identity".into(), MetadataValue::Boolean(true))]),
    ] {
        let mut entries = keys.clone();
        entries.get_mut("ci").unwrap().metadata = metadata;
        assert!(!upload.evaluate(&enforce, &entries).allowed);
    }
    let mut entries = keys.clone();
    entries
        .get_mut("ci")
        .unwrap()
        .metadata
        .insert("owner".into(), MetadataValue::String("alice".into()));
    let custom = UploadVerification {
        identity_field: Some("owner".into()),
        ..enforce
    };
    assert!(upload.evaluate(&custom, &entries).allowed);
}

#[test]
fn enforce_needs_one_valid_matching_signer_but_threshold_counts_all_valid_identities() {
    let alice = key_pair(7);
    let bob = key_pair(8);
    let mut bob_key = registered("bob", &bob);
    bob_key
        .metadata
        .insert("identity".into(), MetadataValue::String("bob".into()));
    let keys = keys(vec![registered("alice", &alice), bob_key]);
    let config = UploadVerification {
        min_signatures: Some(2),
        identity_match: IdentityMatch::Enforce,
        ..config()
    };
    let mut upload = Upload {
        signatures: vec![sign("alice", &alice), sign("bob", &bob)],
        ..Upload::default()
    };
    let result = upload.evaluate(&config, &keys);
    assert!(result.allowed);
    assert!(result.identity_mismatch);
    assert_eq!(result.valid_signatures, 2);
    upload.signatures[0] = sign("alice", &bob);
    let config = UploadVerification {
        min_signatures: Some(1),
        ..config
    };
    assert_eq!(
        upload.evaluate(&config, &keys).reason,
        EvaluationReason::IdentityMismatch
    );
}

#[test]
fn disabled_is_noop_and_ca_is_only_an_exemption_eligibility_claim() {
    let mut upload = Upload {
        signatures: vec![b"malformed".to_vec()],
        ..Upload::default()
    };
    let result = upload.evaluate(&UploadVerification::default(), &BTreeMap::new());
    assert!(result.allowed);
    assert!(result.signatures.is_empty());
    assert_eq!(result.reason, EvaluationReason::VerificationDisabled);
    assert!(!upload.evaluate(&config(), &BTreeMap::new()).allowed);
    // Deliberately not valid CA evidence: the caller must validate the actual content with Nix.
    upload.content_address = Some("not-validated-here".into());
    let enforce = UploadVerification {
        identity_match: IdentityMatch::Enforce,
        ..config()
    };
    let result = upload.evaluate(&enforce, &BTreeMap::new());
    assert!(result.allowed);
    assert!(result.content_addressed_exemption_eligible);
    assert_eq!(result.reason, EvaluationReason::ContentAddressedExemption);
    let no_exemption = UploadVerification {
        exempt_content_addressed: Some(false),
        ..enforce
    };
    assert!(!upload.evaluate(&no_exemption, &BTreeMap::new()).allowed);
}

#[test]
fn signature_key_names_are_bounded_registry_labels() {
    assert_eq!(key_name(b"ci:ignored:payload"), Some("ci"));
    assert_eq!(key_name(b"ci"), None);
    assert_eq!(key_name(b":signature"), None);
    assert_eq!(key_name(b"ci\0:signature"), None);
    let longest = format!("{}:signature", "k".repeat(256));
    assert_eq!(key_name(longest.as_bytes()).unwrap().len(), 256);
    let oversized = format!("{}:signature", "k".repeat(257));
    assert_eq!(key_name(oversized.as_bytes()), None);
}

#[test]
fn malformed_fingerprint_hash_fails_closed_without_error_details() {
    for hash in ["", "abcd", &"g".repeat(64)] {
        let upload = Upload {
            hash: hash.into(),
            ..Upload::default()
        };
        let result = upload.evaluate(&config(), &BTreeMap::new());
        assert!(!result.allowed);
        assert_eq!(result.reason, EvaluationReason::InvalidFingerprint);
        upload.with_info(|info| assert!(fingerprint(info).is_none()));
    }
}
