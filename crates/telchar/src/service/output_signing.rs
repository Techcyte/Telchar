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
