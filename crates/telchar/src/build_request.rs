use std::fmt;
use std::io;

use nix_worker_protocol::BuildDerivationRequest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::deployment::DeploymentConfig;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildRequest {
    derivation_path: Vec<u8>,
    expected_outputs: Vec<(Vec<u8>, Vec<u8>)>,
    input_sources: Vec<Vec<u8>>,
    system: String,
    builder: Vec<u8>,
    arguments: Vec<Vec<u8>>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
}

impl BuildRequest {
    pub fn from_worker_request(
        request: &BuildDerivationRequest,
        deployment: &DeploymentConfig,
    ) -> io::Result<Self> {
        if request.platform().is_empty() || request.platform() != deployment.system().as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported BuildDerivation request",
            ));
        }
        let environment = request.environment();
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
        for output in request.outputs() {
            if environment_value(environment, output.name()) != Some(output.path()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid BuildDerivation request",
                ));
            }
            expected_outputs.push((output.name().to_vec(), output.path().to_vec()));
        }
        Ok(Self {
            derivation_path: request.drv_path().to_vec(),
            expected_outputs,
            input_sources: request.input_sources().to_vec(),
            system: deployment.system().to_owned(),
            builder: request.builder().to_vec(),
            arguments: request.arguments().to_vec(),
            environment: environment.to_vec(),
        })
    }

    pub fn validate_for_execution(&self, deployment: &DeploymentConfig) -> io::Result<()> {
        if self.system != deployment.system()
            || self.derivation_path.is_empty()
            || self.builder.is_empty()
            || derivation_name(&self.derivation_path).is_none()
            || environment_value(&self.environment, b"system") != Some(self.system.as_bytes())
            || environment_value(&self.environment, b"builder") != Some(self.builder.as_slice())
            || environment_value(&self.environment, b"name")
                != derivation_name(&self.derivation_path)
            || self.expected_outputs.is_empty()
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

    pub fn shared_build_key(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"telchar-shared-build-v1\0");
        update_digest_bytes(&mut digest, &self.derivation_path);
        update_digest_pairs(&mut digest, &self.expected_outputs);
        update_digest_values(&mut digest, &self.input_sources);
        update_digest_bytes(&mut digest, self.system.as_bytes());
        update_digest_bytes(&mut digest, &self.builder);
        update_digest_values(&mut digest, &self.arguments);
        update_digest_pairs(&mut digest, &self.environment);
        format!(
            "{}:{:x}",
            String::from_utf8_lossy(&self.derivation_path),
            digest.finalize()
        )
    }

    pub fn derivation_path(&self) -> &[u8] {
        &self.derivation_path
    }

    pub fn expected_outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.expected_outputs
    }

    pub fn input_sources(&self) -> &[Vec<u8>] {
        &self.input_sources
    }

    pub fn system(&self) -> &str {
        &self.system
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

fn update_digest_values(digest: &mut Sha256, values: &[Vec<u8>]) {
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        update_digest_bytes(digest, value);
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
            .field("input_sources", &self.input_sources)
            .field("system", &self.system)
            .field("builder", &self.builder)
            .field("argument_count", &self.arguments.len())
            .field("environment_count", &self.environment.len())
            .finish()
    }
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
