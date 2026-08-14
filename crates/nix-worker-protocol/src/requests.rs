//! Defines bounded store-transfer and build-derivation request models.

use std::io;

use crate::SessionAllocationCharge;

#[derive(Debug, Eq, PartialEq)]
pub struct AddMultipleToStoreRequestError(pub(crate) io::ErrorKind, pub(crate) String);

impl std::fmt::Display for AddMultipleToStoreRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.1)
    }
}

impl std::error::Error for AddMultipleToStoreRequestError {}

impl AddMultipleToStoreRequestError {
    pub const fn kind(&self) -> io::ErrorKind {
        self.0
    }
}

#[derive(Debug)]
pub struct AddMultipleToStorePathInfo {
    pub(crate) path: Vec<u8>,
    pub(crate) deriver: Option<Vec<u8>>,
    pub(crate) nar_hash: Vec<u8>,
    pub(crate) references: Vec<Vec<u8>>,
    pub(crate) registration_time: u64,
    pub(crate) nar_size: u64,
    pub(crate) ultimate: bool,
    pub(crate) signatures: Vec<Vec<u8>>,
    pub(crate) content_address: Option<Vec<u8>>,
    pub(crate) _charges: Vec<SessionAllocationCharge>,
}

impl AddMultipleToStorePathInfo {
    pub fn path(&self) -> &[u8] {
        &self.path
    }
    pub fn deriver(&self) -> Option<&[u8]> {
        self.deriver.as_deref()
    }
    pub fn nar_hash(&self) -> &[u8] {
        &self.nar_hash
    }
    pub fn references(&self) -> &[Vec<u8>] {
        &self.references
    }
    pub const fn registration_time(&self) -> u64 {
        self.registration_time
    }
    pub const fn nar_size(&self) -> u64 {
        self.nar_size
    }
    pub const fn ultimate(&self) -> bool {
        self.ultimate
    }
    pub fn signatures(&self) -> &[Vec<u8>] {
        &self.signatures
    }
    pub fn content_address(&self) -> Option<&[u8]> {
        self.content_address.as_deref()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct AddMultipleToStoreRequest {
    pub(crate) repair: bool,
    pub(crate) dont_check_signatures: bool,
    pub(crate) object_count: usize,
}

impl AddMultipleToStoreRequest {
    pub const fn repair(&self) -> bool {
        self.repair
    }
    pub const fn dont_check_signatures(&self) -> bool {
        self.dont_check_signatures
    }
    pub const fn object_count(&self) -> usize {
        self.object_count
    }
}

pub type EmptyAddMultipleToStoreRequest = AddMultipleToStoreRequest;

#[derive(Debug)]
pub struct BuildDerivationOutput {
    pub(crate) name: Vec<u8>,
    pub(crate) path: Vec<u8>,
    pub(crate) hash_algorithm: Vec<u8>,
    pub(crate) hash: Vec<u8>,
    pub(crate) _charges: BuildDerivationStringCharges,
}

impl BuildDerivationOutput {
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
}

#[derive(Debug)]
pub struct BuildDerivationRequest {
    pub(crate) drv_path: Vec<u8>,
    pub(crate) outputs: Vec<BuildDerivationOutput>,
    pub(crate) input_sources: Vec<Vec<u8>>,
    pub(crate) platform: Vec<u8>,
    pub(crate) builder: Vec<u8>,
    pub(crate) arguments: Vec<Vec<u8>>,
    pub(crate) environment: Vec<(Vec<u8>, Vec<u8>)>,
    pub(crate) build_mode: u64,
    pub(crate) _charges: BuildDerivationCharges,
}

#[derive(Debug)]
pub(crate) struct BuildDerivationStringCharges {
    pub(crate) _charges: Vec<SessionAllocationCharge>,
}

#[derive(Debug)]
pub(crate) struct BuildDerivationCharges {
    pub(crate) _collection_charges: Vec<SessionAllocationCharge>,
    pub(crate) _string_charges: Vec<SessionAllocationCharge>,
}

impl BuildDerivationRequest {
    pub fn drv_path(&self) -> &[u8] {
        &self.drv_path
    }

    pub fn outputs(&self) -> &[BuildDerivationOutput] {
        &self.outputs
    }

    pub fn input_sources(&self) -> &[Vec<u8>] {
        &self.input_sources
    }

    pub fn platform(&self) -> &[u8] {
        &self.platform
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

    pub const fn build_mode(&self) -> u64 {
        self.build_mode
    }
}
