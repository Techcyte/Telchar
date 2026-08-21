//! Validates worker BuildDerivation requests and derives the bounded semantic identity used for shared builds.

use std::collections::BTreeSet;
use std::fmt;
use std::io;

use nix_worker_protocol::BuildDerivationRequest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::BackendTarget;

mod derivation;

const MAXIMUM_REQUIRED_SYSTEM_FEATURES: usize = 64;
const MAXIMUM_REQUIRED_SYSTEM_FEATURE_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputAuthority {
    name: Vec<u8>,
    path: Vec<u8>,
    hash_algorithm: Vec<u8>,
    hash: Vec<u8>,
}

impl OutputAuthority {
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn hash_algorithm(&self) -> &[u8] {
        &self.hash_algorithm
    }

    pub fn hash(&self) -> &[u8] {
        &self.hash
    }

    pub fn expected_content_address(&self) -> Option<String> {
        if self.hash_algorithm.is_empty() {
            return None;
        }
        let method = if self.hash_algorithm.starts_with(b"r:") {
            "fixed:r:"
        } else {
            "fixed:"
        };
        let algorithm = self
            .hash_algorithm
            .strip_prefix(b"r:")
            .unwrap_or(&self.hash_algorithm);
        let hash = decode_hex(&self.hash)?;
        Some(format!(
            "{method}{}:{}",
            std::str::from_utf8(algorithm).ok()?,
            encode_nix_base32(&hash)
        ))
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildRequest {
    derivation_path: Vec<u8>,
    expected_outputs: Vec<(Vec<u8>, Vec<u8>)>,
    output_authorities: Vec<OutputAuthority>,
    input_sources: Vec<Vec<u8>>,
    system: String,
    required_system_features: Vec<String>,
    builder: Vec<u8>,
    arguments: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
}

impl BuildRequest {
    pub fn from_stored_derivation(
        derivation_path: &[u8],
        contents: &[u8],
        backends: &[BackendTarget],
    ) -> io::Result<Self> {
        let stored = derivation::parse(contents).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("stored derivation parse failed: {error}"),
            )
        })?;
        let required_system_features = required_system_features(&stored.environment)?;
        let required_features = required_system_features
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let system = std::str::from_utf8(&stored.system).map_err(|_| unsupported_request())?;
        if system.is_empty()
            || !backends
                .iter()
                .any(|backend| backend.supports(system, &required_features))
            || environment_value(&stored.environment, b"system") != Some(stored.system.as_slice())
            || environment_value(&stored.environment, b"builder") != Some(stored.builder.as_slice())
            || environment_value(&stored.environment, b"name") != derivation_name(derivation_path)
        {
            return Err(unsupported_request());
        }
        let mut expected_outputs = Vec::with_capacity(stored.outputs.len());
        let mut output_authorities = Vec::with_capacity(stored.outputs.len());
        for (name, path, hash_algorithm, hash) in stored.outputs {
            if environment_value(&stored.environment, &name) != Some(path.as_slice()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid stored derivation",
                ));
            }
            expected_outputs.push((name.clone(), path.clone()));
            output_authorities.push(OutputAuthority {
                name,
                path,
                hash_algorithm,
                hash,
            });
        }
        let mut input_sources = stored.input_sources;
        input_sources.extend(stored.input_derivations);
        let request = Self {
            derivation_path: derivation_path.to_vec(),
            expected_outputs,
            output_authorities,
            input_sources,
            system: system.to_owned(),
            required_system_features,
            builder: stored.builder,
            arguments: stored.arguments,
            environment: stored.environment,
        };
        request.validate_for_execution()?;
        Ok(request)
    }

    pub fn from_worker_request(
        request: &BuildDerivationRequest,
        backends: &[BackendTarget],
    ) -> io::Result<Self> {
        let environment = request.environment();
        let required_system_features = required_system_features(environment)?;
        let required_features = required_system_features
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let system = std::str::from_utf8(request.platform()).map_err(|_| unsupported_request())?;
        if request.platform().is_empty()
            || !backends
                .iter()
                .any(|backend| backend.supports(system, &required_features))
        {
            return Err(unsupported_request());
        }
        let expected_name = derivation_name(request.drv_path()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid BuildDerivation request",
            )
        })?;
        if environment_value(environment, b"system") != Some(request.platform())
            || environment_value(environment, b"builder") != Some(request.builder())
            || environment_value(environment, b"name") != Some(expected_name)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid BuildDerivation request",
            ));
        }
        let mut expected_outputs = Vec::with_capacity(request.outputs().len());
        let mut output_authorities = Vec::with_capacity(request.outputs().len());
        for output in request.outputs() {
            if environment_value(environment, output.name()) != Some(output.path()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid BuildDerivation request",
                ));
            }
            expected_outputs.push((output.name().to_vec(), output.path().to_vec()));
            output_authorities.push(OutputAuthority {
                name: output.name().to_vec(),
                path: output.path().to_vec(),
                hash_algorithm: output.hash_algorithm().to_vec(),
                hash: output.hash().to_vec(),
            });
        }
        Ok(Self {
            derivation_path: request.drv_path().to_vec(),
            expected_outputs,
            output_authorities,
            input_sources: request.input_sources().to_vec(),
            system: system.to_owned(),
            required_system_features,
            builder: request.builder().to_vec(),
            arguments: request.arguments().to_vec(),
            environment: environment.to_vec(),
        })
    }

    pub fn validate_for_execution(&self) -> io::Result<()> {
        if self.derivation_path.is_empty()
            || self.builder.is_empty()
            || derivation_name(&self.derivation_path).is_none()
            || environment_value(&self.environment, b"system") != Some(self.system.as_bytes())
            || environment_value(&self.environment, b"builder") != Some(self.builder.as_slice())
            || environment_value(&self.environment, b"name")
                != derivation_name(&self.derivation_path)
            || self.expected_outputs.is_empty()
            || self.output_authorities.len() != self.expected_outputs.len()
            || self
                .output_authorities
                .iter()
                .zip(&self.expected_outputs)
                .any(|(authority, (name, path))| authority.name != *name || authority.path != *path)
            || self.expected_outputs.iter().any(|(name, path)| {
                name.is_empty()
                    || path.is_empty()
                    || environment_value(&self.environment, name) != Some(path.as_slice())
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid local execution specification",
            ));
        }
        Ok(())
    }

    pub fn shared_build_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"telchar-shared-build-v2\0");
        update_digest_bytes(&mut digest, &self.derivation_path);
        update_digest_output_authorities(&mut digest, &self.output_authorities);
        update_digest_values(&mut digest, &self.input_sources);
        update_digest_bytes(&mut digest, self.system.as_bytes());
        update_digest_strings(&mut digest, &self.required_system_features);
        update_digest_bytes(&mut digest, &self.builder);
        update_digest_values(&mut digest, &self.arguments);
        update_digest_pairs(&mut digest, &self.environment);
        digest.finalize().into()
    }

    pub fn shared_build_key(&self) -> String {
        let digest = self
            .shared_build_digest()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!(
            "{}:{digest}",
            String::from_utf8_lossy(&self.derivation_path)
        )
    }

    pub fn derivation_path(&self) -> &[u8] {
        &self.derivation_path
    }

    pub fn expected_outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.expected_outputs
    }

    pub fn output_authorities(&self) -> &[OutputAuthority] {
        &self.output_authorities
    }

    pub fn input_sources(&self) -> &[Vec<u8>] {
        &self.input_sources
    }

    pub fn system(&self) -> &str {
        &self.system
    }

    pub fn required_system_features(&self) -> &[String] {
        &self.required_system_features
    }

    pub fn builder(&self) -> &[u8] {
        &self.builder
    }

    pub fn arguments(&self) -> &[Vec<u8>] {
        &self.arguments
    }

    pub fn environment(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.environment
    }
}

fn decode_hex(value: &[u8]) -> Option<Vec<u8>> {
    value
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)? as u8;
            let low = (pair[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect()
}

fn encode_nix_base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let length = (bytes.len() * 8).div_ceil(5);
    let mut encoded = String::with_capacity(length);
    for index in (0..length).rev() {
        let bit = index * 5;
        let byte = bit / 8;
        let shift = bit % 8;
        let value = (bytes[byte] >> shift)
            | if shift == 0 {
                0
            } else {
                bytes.get(byte + 1).copied().unwrap_or_default() << (8 - shift)
            };
        encoded.push(ALPHABET[(value & 0x1f) as usize] as char);
    }
    encoded
}

fn update_digest_strings(digest: &mut Sha256, values: &[String]) {
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        update_digest_bytes(digest, value.as_bytes());
    }
}

fn update_digest_values(digest: &mut Sha256, values: &[Vec<u8>]) {
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        update_digest_bytes(digest, value);
    }
}

fn update_digest_output_authorities(digest: &mut Sha256, values: &[OutputAuthority]) {
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        update_digest_bytes(digest, &value.name);
        update_digest_bytes(digest, &value.path);
        update_digest_bytes(digest, &value.hash_algorithm);
        update_digest_bytes(digest, &value.hash);
    }
}

fn update_digest_pairs(digest: &mut Sha256, values: &[(Vec<u8>, Vec<u8>)]) {
    digest.update((values.len() as u64).to_le_bytes());
    for (name, value) in values {
        update_digest_bytes(digest, name);
        update_digest_bytes(digest, value);
    }
}

fn update_digest_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

impl fmt::Debug for BuildRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuildRequest")
            .field("derivation_path", &self.derivation_path)
            .field("expected_outputs", &self.expected_outputs)
            .field("output_authorities", &self.output_authorities)
            .field("input_sources", &self.input_sources)
            .field("system", &self.system)
            .field("required_system_features", &self.required_system_features)
            .field("builder", &self.builder)
            .field("argument_count", &self.arguments.len())
            .field("environment_count", &self.environment.len())
            .finish()
    }
}

fn required_system_features(environment: &[(Vec<u8>, Vec<u8>)]) -> io::Result<Vec<String>> {
    let Some(value) = environment_value(environment, b"requiredSystemFeatures") else {
        return Ok(Vec::new());
    };
    let value = std::str::from_utf8(value).map_err(|_| unsupported_request())?;
    let mut features = BTreeSet::new();
    for feature in value.split_ascii_whitespace() {
        if features.len() >= MAXIMUM_REQUIRED_SYSTEM_FEATURES
            || feature.is_empty()
            || feature.len() > MAXIMUM_REQUIRED_SYSTEM_FEATURE_BYTES
            || !feature.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+')
            })
            || !features.insert(feature.to_owned())
        {
            return Err(unsupported_request());
        }
    }
    Ok(features.into_iter().collect())
}

fn unsupported_request() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "unsupported BuildDerivation request",
    )
}

fn environment_value<'a>(environment: &'a [(Vec<u8>, Vec<u8>)], key: &[u8]) -> Option<&'a [u8]> {
    environment
        .iter()
        .find(|(environment_key, _)| environment_key == key)
        .map(|(_, value)| value.as_slice())
}

fn derivation_name(path: &[u8]) -> Option<&[u8]> {
    let name = path.rsplit(|byte| *byte == b'/').next()?;
    let name = name.strip_suffix(b".drv")?;
    name.get(33..)
}
