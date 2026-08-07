use std::io;

use nix_worker_protocol::BuildDerivationRequest;

use crate::deployment::DeploymentConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
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
        _request: &BuildDerivationRequest,
        _deployment: &DeploymentConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "BuildDerivation admission is unavailable",
        ))
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
