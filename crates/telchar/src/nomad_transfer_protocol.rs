use std::collections::BTreeSet;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 4] = b"TLNW";
const HEADER_BYTES: usize = 16;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Authentication {
    pub backend: String,
    pub namespace: String,
    pub job_id: String,
    pub allocation_id: String,
    pub task: String,
    pub shared_build_digest: String,
    pub proof: AuthenticationProof,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthenticationProof {
    WorkloadIdentity {
        token: String,
    },
    Hmac {
        capability: String,
        expiry: u64,
        nonce: String,
        body_digest: String,
        signature: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputManifest {
    pub derivation_path: String,
    pub paths: Vec<PathManifestEntry>,
    pub outputs: Vec<String>,
}

impl InputManifest {
    pub fn validate(&self, maximum_paths: usize, maximum_nar_bytes: u64) -> io::Result<()> {
        validate_store_path(&self.derivation_path, true)?;
        if self.paths.is_empty()
            || self.paths.len() > maximum_paths
            || self.outputs.is_empty()
            || self.outputs.len() > maximum_paths
        {
            return Err(invalid_data(
                "Nomad transfer manifest path count is invalid",
            ));
        }
        let mut admitted = BTreeSet::new();
        for entry in &self.paths {
            entry.validate(maximum_paths, maximum_nar_bytes)?;
            if !admitted.insert(entry.path.as_str()) {
                return Err(invalid_data(
                    "Nomad transfer manifest contains duplicate path",
                ));
            }
        }
        for entry in &self.paths {
            if entry
                .references
                .iter()
                .any(|reference| !admitted.contains(reference.as_str()))
            {
                return Err(invalid_data(
                    "Nomad transfer manifest reference is not admitted",
                ));
            }
        }
        let mut outputs = BTreeSet::new();
        for output in &self.outputs {
            validate_store_path(output, false)?;
            if !outputs.insert(output.as_str()) {
                return Err(invalid_data(
                    "Nomad transfer manifest contains duplicate output",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathManifestEntry {
    pub path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub deriver: Option<String>,
}

impl PathManifestEntry {
    fn validate(&self, maximum_references: usize, maximum_nar_bytes: u64) -> io::Result<()> {
        validate_store_path(&self.path, self.path.ends_with(".drv"))?;
        validate_nar_hash(&self.nar_hash)?;
        if self.nar_size > maximum_nar_bytes || self.references.len() > maximum_references {
            return Err(invalid_data("Nomad transfer path metadata exceeds limit"));
        }
        let mut references = BTreeSet::new();
        for reference in &self.references {
            validate_store_path(reference, reference.ends_with(".drv"))?;
            if !references.insert(reference) {
                return Err(invalid_data(
                    "Nomad transfer path contains duplicate reference",
                ));
            }
        }
        if let Some(deriver) = &self.deriver {
            validate_store_path(deriver, true)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathSet {
    pub paths: Vec<String>,
}

impl PathSet {
    pub fn validate_against(&self, admitted: &[String], maximum_paths: usize) -> io::Result<()> {
        if self.paths.len() > maximum_paths {
            return Err(invalid_data("Nomad transfer path set exceeds limit"));
        }
        let admitted = admitted.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let mut paths = BTreeSet::new();
        for path in &self.paths {
            validate_store_path(path, path.ends_with(".drv"))?;
            if !admitted.contains(path.as_str()) || !paths.insert(path) {
                return Err(invalid_data("Nomad transfer path is not admitted"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NarMetadata {
    pub path: String,
    pub nar_hash: String,
    pub nar_size: u64,
}

impl NarMetadata {
    pub fn validate_against(&self, admitted: &[String], maximum_nar_bytes: u64) -> io::Result<()> {
        validate_store_path(&self.path, self.path.ends_with(".drv"))?;
        validate_nar_hash(&self.nar_hash)?;
        if self.nar_size > maximum_nar_bytes || !admitted.iter().any(|path| path == &self.path) {
            return Err(invalid_data("Nomad transfer NAR metadata is not admitted"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildStarted {
    pub derivation_path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogChunk {
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputReceipt {
    pub path: String,
    pub accepted: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildOutcome {
    Built,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildResultMetadata {
    pub outcome: BuildOutcome,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum FrameKind {
    Authenticate = 1,
    InputManifest = 2,
    ValidPaths = 3,
    InputRequest = 4,
    InputNar = 5,
    BuildStarted = 6,
    LogChunk = 7,
    OutputMetadata = 8,
    OutputNar = 9,
    OutputReceipt = 10,
    BuildResult = 11,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    WorkerToGateway,
    GatewayToWorker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    AwaitingAuthentication,
    AwaitingManifest,
    ResolvingInputs,
    Building,
    CollectingOutputs,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolSession {
    phase: Phase,
}

impl ProtocolSession {
    pub const fn new() -> Self {
        Self {
            phase: Phase::AwaitingAuthentication,
        }
    }

    pub fn accept(&mut self, direction: Direction, kind: FrameKind) -> io::Result<()> {
        let next = match (self.phase, direction, kind) {
            (
                Phase::AwaitingAuthentication,
                Direction::WorkerToGateway,
                FrameKind::Authenticate,
            ) => Phase::AwaitingManifest,
            (Phase::AwaitingManifest, Direction::GatewayToWorker, FrameKind::InputManifest) => {
                Phase::ResolvingInputs
            }
            (Phase::ResolvingInputs, Direction::WorkerToGateway, FrameKind::ValidPaths)
            | (Phase::ResolvingInputs, Direction::WorkerToGateway, FrameKind::InputRequest)
            | (Phase::ResolvingInputs, Direction::GatewayToWorker, FrameKind::InputNar) => {
                Phase::ResolvingInputs
            }
            (Phase::ResolvingInputs, Direction::WorkerToGateway, FrameKind::BuildStarted) => {
                Phase::Building
            }
            (Phase::Building, Direction::WorkerToGateway, FrameKind::LogChunk) => Phase::Building,
            (Phase::Building, Direction::WorkerToGateway, FrameKind::OutputMetadata) => {
                Phase::CollectingOutputs
            }
            (Phase::CollectingOutputs, Direction::WorkerToGateway, FrameKind::OutputMetadata)
            | (Phase::CollectingOutputs, Direction::WorkerToGateway, FrameKind::OutputNar)
            | (Phase::CollectingOutputs, Direction::GatewayToWorker, FrameKind::OutputReceipt) => {
                Phase::CollectingOutputs
            }
            (Phase::CollectingOutputs, Direction::WorkerToGateway, FrameKind::BuildResult) => {
                Phase::Complete
            }
            _ => {
                return Err(invalid_data(
                    "Nomad transfer frame is invalid for protocol phase",
                ))
            }
        };
        self.phase = next;
        Ok(())
    }

    pub fn is_complete(self) -> bool {
        self.phase == Phase::Complete
    }
}

impl Default for ProtocolSession {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<u16> for FrameKind {
    type Error = io::Error;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Authenticate),
            2 => Ok(Self::InputManifest),
            3 => Ok(Self::ValidPaths),
            4 => Ok(Self::InputRequest),
            5 => Ok(Self::InputNar),
            6 => Ok(Self::BuildStarted),
            7 => Ok(Self::LogChunk),
            8 => Ok(Self::OutputMetadata),
            9 => Ok(Self::OutputNar),
            10 => Ok(Self::OutputReceipt),
            11 => Ok(Self::BuildResult),
            _ => Err(invalid_data("Nomad transfer frame kind is unsupported")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimits {
    maximum_metadata_bytes: usize,
    maximum_payload_bytes: usize,
}

impl ProtocolLimits {
    pub const fn new(maximum_metadata_bytes: usize, maximum_payload_bytes: usize) -> Self {
        Self {
            maximum_metadata_bytes,
            maximum_payload_bytes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    kind: FrameKind,
    metadata: Vec<u8>,
    payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: FrameKind, metadata: Vec<u8>, payload: Vec<u8>) -> Self {
        Self {
            kind,
            metadata,
            payload,
        }
    }

    pub fn kind(&self) -> FrameKind {
        self.kind
    }

    pub fn metadata(&self) -> &[u8] {
        &self.metadata
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

pub fn encode_metadata<T: Serialize>(value: &T, maximum_bytes: usize) -> io::Result<Vec<u8>> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| invalid_input("Nomad transfer frame metadata is invalid"))?;
    if encoded.len() > maximum_bytes {
        return Err(invalid_input("Nomad transfer frame metadata exceeds limit"));
    }
    Ok(encoded)
}

pub fn decode_metadata<T: DeserializeOwned>(encoded: &[u8], maximum_bytes: usize) -> io::Result<T> {
    if encoded.len() > maximum_bytes {
        return Err(invalid_data("Nomad transfer frame metadata exceeds limit"));
    }
    serde_json::from_slice(encoded)
        .map_err(|_| invalid_data("Nomad transfer frame metadata is invalid"))
}

pub fn write_frame(
    output: &mut impl Write,
    frame: &Frame,
    limits: ProtocolLimits,
) -> io::Result<()> {
    validate_lengths(frame.metadata.len(), frame.payload.len(), limits, false)?;
    let metadata_length = u32::try_from(frame.metadata.len())
        .map_err(|_| invalid_input("Nomad transfer frame metadata exceeds limit"))?;
    let payload_length = u32::try_from(frame.payload.len())
        .map_err(|_| invalid_input("Nomad transfer frame payload exceeds limit"))?;

    output.write_all(MAGIC)?;
    output.write_all(&PROTOCOL_VERSION.to_be_bytes())?;
    output.write_all(&(frame.kind as u16).to_be_bytes())?;
    output.write_all(&metadata_length.to_be_bytes())?;
    output.write_all(&payload_length.to_be_bytes())?;
    output.write_all(&frame.metadata)?;
    output.write_all(&frame.payload)
}

pub fn read_frame(input: &mut impl Read, limits: ProtocolLimits) -> io::Result<Frame> {
    let mut header = [0_u8; HEADER_BYTES];
    input.read_exact(&mut header)?;
    if &header[..4] != MAGIC {
        return Err(invalid_data("Nomad transfer frame magic is invalid"));
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != PROTOCOL_VERSION {
        return Err(invalid_data(
            "Nomad transfer protocol version is unsupported",
        ));
    }
    let kind = FrameKind::try_from(u16::from_be_bytes([header[6], header[7]]))?;
    let metadata_length = usize::try_from(u32::from_be_bytes([
        header[8], header[9], header[10], header[11],
    ]))
    .map_err(|_| invalid_data("Nomad transfer frame metadata exceeds limit"))?;
    let payload_length = usize::try_from(u32::from_be_bytes([
        header[12], header[13], header[14], header[15],
    ]))
    .map_err(|_| invalid_data("Nomad transfer frame payload exceeds limit"))?;
    validate_lengths(metadata_length, payload_length, limits, true)?;

    let mut metadata = vec![0; metadata_length];
    input.read_exact(&mut metadata)?;
    let mut payload = vec![0; payload_length];
    input.read_exact(&mut payload)?;
    Ok(Frame::new(kind, metadata, payload))
}

fn validate_lengths(
    metadata_length: usize,
    payload_length: usize,
    limits: ProtocolLimits,
    wire_input: bool,
) -> io::Result<()> {
    let error = |message| {
        if wire_input {
            invalid_data(message)
        } else {
            invalid_input(message)
        }
    };
    if metadata_length > limits.maximum_metadata_bytes {
        return Err(error("Nomad transfer frame metadata exceeds limit"));
    }
    if payload_length > limits.maximum_payload_bytes {
        return Err(error("Nomad transfer frame payload exceeds limit"));
    }
    Ok(())
}

fn validate_nar_hash(hash: &str) -> io::Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_data("Nomad transfer NAR hash is invalid"));
    }
    Ok(())
}

fn validate_store_path(path: &str, deriver: bool) -> io::Result<()> {
    const STORE_DIRECTORY: &str = "/nix/store/";
    const HASH_LENGTH: usize = 32;
    const MAXIMUM_BASE_NAME_LENGTH: usize = 211;
    const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

    let Some(base) = path.strip_prefix(STORE_DIRECTORY) else {
        return Err(invalid_data("Nomad transfer store path is invalid"));
    };
    if base.len() > MAXIMUM_BASE_NAME_LENGTH
        || base.as_bytes().get(HASH_LENGTH) != Some(&b'-')
        || base.contains('/')
    {
        return Err(invalid_data("Nomad transfer store path is invalid"));
    }
    let Some(hash) = base.get(..HASH_LENGTH) else {
        return Err(invalid_data("Nomad transfer store path is invalid"));
    };
    let Some(name) = base.get(HASH_LENGTH + 1..) else {
        return Err(invalid_data("Nomad transfer store path is invalid"));
    };
    if !hash.bytes().all(|byte| HASH_ALPHABET.contains(&byte))
        || name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
        || name.ends_with(".drv") != deriver
    {
        return Err(invalid_data("Nomad transfer store path is invalid"));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
