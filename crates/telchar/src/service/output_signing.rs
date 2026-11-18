//! Optional provenance signatures for live executor completion, never uploads or recovery.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use aws_lc_rs::signature::Ed25519KeyPair;
use base64::{Engine, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

use crate::build::BuildRequest;
use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};
use crate::store::promotion::RegisteredPathInfo;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningFailurePolicy {
    #[default]
    Warn,
    FailBuild,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SigningConfig {
    key_file: PathBuf,
    failure_policy: SigningFailurePolicy,
}

impl SigningConfig {
    pub(crate) fn new(key_file: PathBuf, failure_policy: SigningFailurePolicy) -> Self {
        Self {
            key_file,
            failure_policy,
        }
    }

    pub fn failure_policy(&self) -> SigningFailurePolicy {
        self.failure_policy
    }

    /// Load once before admission. Recheck the opened descriptor, including rotated symlink targets.
    pub fn load(&self, endpoint: GatewayStoreEndpoint) -> io::Result<OutputSigner> {
        if !self.key_file.is_absolute() {
            return Err(key_error());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&self.key_file)
            .map_err(|_| key_error())?;
        let metadata = file.metadata().map_err(|_| key_error())?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
            || metadata.len() > 4096
        {
            return Err(key_error());
        }
        let mut secret = Zeroizing::new(String::new());
        file.take(4097)
            .read_to_string(&mut secret)
            .map_err(|_| key_error())?;
        if secret.len() > 4096 {
            return Err(key_error());
        }
        let (name, encoded) = secret.trim().split_once(':').ok_or_else(key_error)?;
        if name.is_empty()
            || name.len() > 256
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(key_error());
        }
        let bytes = Zeroizing::new(STANDARD.decode(encoded).map_err(|_| key_error())?);
        if bytes.len() != 64 {
            return Err(key_error());
        }
        let key = Ed25519KeyPair::from_seed_and_public_key(&bytes[..32], &bytes[32..])
            .map_err(|_| key_error())?;
        Ok(OutputSigner {
            key: Arc::new(key),
            name: name.to_owned(),
            endpoint,
            failure_policy: self.failure_policy,
        })
    }
}

fn key_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "signing key file is invalid")
}
fn signing_error() -> io::Error {
    io::Error::other("executor output signing failed")
}

#[derive(Clone)]
pub struct OutputSigner {
    key: Arc<Ed25519KeyPair>,
    name: String,
    endpoint: GatewayStoreEndpoint,
    failure_policy: SigningFailurePolicy,
}

impl OutputSigner {
    /// Only pass authoritative metadata after complete gateway NAR and admitted-authority validation.
    /// Checks authority before each external write; fencing cannot undo an already committed signature.
    pub(crate) fn sign_validated_outputs(
        &self,
        outputs: &[RegisteredPathInfo],
        mut check_authority: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let result = (|| {
            check_authority()?;
            let mut connection = GatewayStoreConnection::connect(&self.endpoint)?;
            for output in outputs {
                check_authority()?;
                let fingerprint = fingerprint(output)?;
                let signature = format!(
                    "{}:{}",
                    self.name,
                    STANDARD.encode(self.key.sign(&fingerprint).as_ref())
                );
                connection.add_signatures(
                    output.path.as_os_str().as_encoded_bytes(),
                    &[signature.into_bytes()],
                )?;
            }
            Ok::<_, io::Error>(())
        })();
        match result {
            Ok(()) => {
                crate::service::metrics::output_signing_finished("succeeded");
                Ok(())
            }
            Err(_) => {
                crate::service::metrics::output_signing_finished("failed");
                tracing::warn!(
                    event = "executor.output_signing.failed",
                    "executor output signing failed"
                );
                match self.failure_policy {
                    SigningFailurePolicy::Warn => Ok(()),
                    SigningFailurePolicy::FailBuild => Err(signing_error()),
                }
            }
        }
    }
}

/// Mandatory validation shared with session completion; never subject to signing's fail-open policy.
pub(crate) fn validate_outputs(
    admitted: &BuildRequest,
    backend: &mut dyn crate::store::export::StoreExportBackend,
) -> io::Result<Vec<RegisteredPathInfo>> {
    admitted
        .expected_outputs()
        .iter()
        .map(|(_, path)| {
            let path_text = std::str::from_utf8(path).map_err(|_| signing_error())?;
            let metadata = crate::store::export::validate_store_output(
                std::path::Path::new(path_text),
                backend,
            )?;
            if metadata.path.as_os_str().as_encoded_bytes() != path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "build output authority mismatch",
                ));
            }
            let authority = admitted
                .output_authorities()
                .iter()
                .find(|authority| authority.path() == path)
                .ok_or_else(signing_error)?;
            if metadata.content_address.as_deref()
                != authority.expected_content_address().as_deref()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "build output content address mismatch",
                ));
            }
            Ok(metadata)
        })
        .collect()
}

fn fingerprint(info: &RegisteredPathInfo) -> io::Result<Vec<u8>> {
    let path = info.path.to_str().ok_or_else(signing_error)?;
    let mut references = info
        .references
        .iter()
        .map(|path| path.to_str().ok_or_else(signing_error))
        .collect::<io::Result<Vec<_>>>()?;
    references.sort_unstable();
    references.dedup();
    Ok(format!(
        "1;{path};sha256:{};{};{}",
        crate::build::encode_nix_base32(&info.nar_hash),
        info.nar_size,
        references.join(",")
    )
    .into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ED25519, KeyPair, UnparsedPublicKey};

    fn config(root: &std::path::Path, policy: SigningFailurePolicy) -> SigningConfig {
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let mut bytes = vec![7; 32];
        bytes.extend(key.public_key().as_ref());
        let path = root.join("key");
        fs::write(&path, format!("fixture:{}", STANDARD.encode(bytes))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        SigningConfig::new(path, policy)
    }

    fn metadata() -> RegisteredPathInfo {
        RegisteredPathInfo {
            path: PathBuf::from("/nix/store/00000000000000000000000000000000-output"),
            nar_hash: [0; 32],
            nar_size: 136,
            references: vec![
                PathBuf::from("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b"),
                PathBuf::from("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a"),
                PathBuf::from("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b"),
            ],
            deriver: None,
            content_address: None,
        }
    }

    #[test]
    fn fingerprint_uses_nix_base32_and_sorted_deduplicated_references() {
        let info = metadata();
        assert_eq!(
            String::from_utf8(fingerprint(&info).unwrap()).unwrap(),
            format!(
                "1;{};sha256:{};136;/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a,/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b",
                info.path.display(),
                "0".repeat(52)
            )
        );
    }

    #[test]
    fn signing_key_checks_opened_permissions_size_and_key_consistency_without_disclosure() {
        let root = tempfile::tempdir().unwrap();
        let cfg = config(root.path(), SigningFailurePolicy::Warn);
        let endpoint = GatewayStoreEndpoint::parse("unix:///missing-signing-store.sock").unwrap();
        let signer = cfg.load(endpoint.clone()).unwrap();
        let message = fingerprint(&metadata()).unwrap();
        UnparsedPublicKey::new(&ED25519, signer.key.public_key().as_ref())
            .verify(&message, signer.key.sign(&message).as_ref())
            .unwrap();
        for secret in [
            "private-secret-sentinel".to_owned(),
            format!("fixture:{}", STANDARD.encode([7; 64])),
            "x".repeat(4097),
        ] {
            fs::write(&cfg.key_file, &secret).unwrap();
            assert_eq!(
                cfg.load(endpoint.clone()).err().unwrap().to_string(),
                "signing key file is invalid"
            );
        }
        let cfg = config(root.path(), SigningFailurePolicy::Warn);
        fs::set_permissions(&cfg.key_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(cfg.load(endpoint).is_err());
    }

    #[test]
    fn partial_multi_output_write_failure_is_not_replayed_and_obeys_live_policy() {
        use nix_worker_protocol::{
            CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST,
            write_worker_byte_string, write_worker_integer,
        };
        use std::os::unix::net::UnixListener;
        fn word(stream: &mut impl Read) -> u64 {
            let mut bytes = [0; 8];
            stream.read_exact(&mut bytes).unwrap();
            u64::from_le_bytes(bytes)
        }
        fn string(stream: &mut impl Read) -> Vec<u8> {
            let len = word(stream) as usize;
            assert!(len < 4096);
            let mut bytes = vec![0; len];
            stream.read_exact(&mut bytes).unwrap();
            let mut padding = [0; 7];
            stream
                .read_exact(&mut padding[..(8 - len % 8) % 8])
                .unwrap();
            bytes
        }
        use std::io::Write;
        for policy in [SigningFailurePolicy::Warn, SigningFailurePolicy::FailBuild] {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("store.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let signer = config(root.path(), policy)
                .load(GatewayStoreEndpoint::parse(&format!("unix://{}", socket.display())).unwrap())
                .unwrap();
            let mut second = metadata();
            second.path = "/nix/store/11111111111111111111111111111111-second".into();
            let outputs = vec![metadata(), second];
            let expected = outputs.clone();
            let public = signer.key.public_key().as_ref().to_vec();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                assert_eq!(word(&mut stream), CLIENT_WORKER_MAGIC);
                assert_eq!(word(&mut stream), LATEST_WORKER_VERSION.to_wire());
                let mut greeting = vec![];
                write_worker_integer(&mut greeting, SERVER_WORKER_MAGIC);
                write_worker_integer(&mut greeting, LATEST_WORKER_VERSION.to_wire());
                stream.write_all(&greeting).unwrap();
                assert_eq!(word(&mut stream), 0);
                stream.write_all(&0u64.to_le_bytes()).unwrap();
                assert_eq!(word(&mut stream), 0);
                assert_eq!(word(&mut stream), 0);
                let mut greeting = vec![];
                write_worker_byte_string(&mut greeting, b"2.34.9");
                write_worker_integer(&mut greeting, 1);
                write_worker_integer(&mut greeting, STDERR_LAST);
                stream.write_all(&greeting).unwrap();
                for (index, output) in expected.iter().enumerate() {
                    assert_eq!(word(&mut stream), 37);
                    assert_eq!(
                        string(&mut stream),
                        output.path.as_os_str().as_encoded_bytes()
                    );
                    assert_eq!(word(&mut stream), 1);
                    let signature = String::from_utf8(string(&mut stream)).unwrap();
                    let bytes = STANDARD
                        .decode(signature.strip_prefix("fixture:").unwrap())
                        .unwrap();
                    UnparsedPublicKey::new(&ED25519, &public)
                        .verify(&fingerprint(output).unwrap(), &bytes)
                        .unwrap();
                    if index == 0 {
                        stream.write_all(&STDERR_LAST.to_le_bytes()).unwrap();
                        stream.write_all(&1u64.to_le_bytes()).unwrap();
                    }
                    // Second operation disconnects after receiving the signature: write outcome is unknown.
                }
            });
            let result = signer.sign_validated_outputs(&outputs, || Ok(()));
            server.join().unwrap();
            assert_eq!(result.is_err(), policy == SigningFailurePolicy::FailBuild);
        }
    }

    #[test]
    fn runtime_failure_is_redacted_and_fail_build_is_opt_in() {
        let root = tempfile::tempdir().unwrap();
        for policy in [SigningFailurePolicy::Warn, SigningFailurePolicy::FailBuild] {
            let signer = config(root.path(), policy)
                .load(GatewayStoreEndpoint::parse("unix:///missing-signing-store.sock").unwrap())
                .unwrap();
            let mut checked = false;
            let result = signer.sign_validated_outputs(&[metadata()], || {
                checked = true;
                Ok(())
            });
            assert!(checked);
            match policy {
                SigningFailurePolicy::Warn => assert!(result.is_ok()),
                SigningFailurePolicy::FailBuild => assert_eq!(
                    result.unwrap_err().to_string(),
                    "executor output signing failed"
                ),
            }
        }
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::store::export::{StoreExportBackend, StoreExportRequest};
    use std::io::Write;

    struct Export {
        metadata: RegisteredPathInfo,
        nar: Vec<u8>,
    }
    impl StoreExportBackend for Export {
        fn store_uri(&self) -> &str {
            "test"
        }
        fn query_path_info(&mut self, _: &std::path::Path) -> io::Result<RegisteredPathInfo> {
            Ok(self.metadata.clone())
        }
        fn export_nar(
            &mut self,
            _: &StoreExportRequest,
            _: u64,
            sink: &mut dyn Write,
        ) -> io::Result<()> {
            sink.write_all(&self.nar)
        }
    }

    #[test]
    fn signing_prerequisite_rejects_corrupt_nar_wrong_path_and_content_address() {
        use sha2::{Digest, Sha256};
        let path = "/nix/store/00000000000000000000000000000000-output";
        let derivation = format!(
            r#"Derive([("out","{path}","","")],[],[],"x86_64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","test"),("out","{path}"),("system","x86_64-linux")])"#
        );
        let target = crate::backend::BackendTarget::new(
            "test",
            crate::backend::BackendKind::Nomad,
            "x86_64-linux",
            [] as [&str; 0],
        )
        .unwrap();
        let request = BuildRequest::from_stored_derivation(
            b"/nix/store/00000000000000000000000000000000-test.drv",
            derivation.as_bytes(),
            &[target],
        )
        .unwrap();
        let mut nar = Vec::new();
        for part in [
            b"nix-archive-1".as_slice(),
            b"(",
            b"type",
            b"regular",
            b"contents",
            b"test",
            b")",
        ] {
            nix_worker_protocol::write_worker_byte_string(&mut nar, part);
        }
        let metadata = RegisteredPathInfo {
            path: path.into(),
            nar_hash: Sha256::digest(&nar).into(),
            nar_size: nar.len() as u64,
            references: vec![],
            deriver: None,
            content_address: None,
        };
        let mut export = Export {
            metadata: metadata.clone(),
            nar: nar.clone(),
        };
        assert!(validate_outputs(&request, &mut export).is_ok());
        for mutation in 0..3 {
            let mut export = Export {
                metadata: metadata.clone(),
                nar: nar.clone(),
            };
            match mutation {
                0 => export.metadata.nar_hash = [0; 32],
                1 => {
                    export.metadata.path =
                        "/nix/store/11111111111111111111111111111111-other".into()
                }
                _ => export.metadata.content_address = Some("fixed:r:sha256:bad".into()),
            }
            assert!(
                validate_outputs(&request, &mut export).is_err(),
                "mutation {mutation} must not reach signing"
            );
        }
    }
}
