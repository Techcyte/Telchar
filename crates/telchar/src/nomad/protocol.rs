//! Defines the bounded versioned TLNW messages exchanged with Nomad allocation workers.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::build::BuildRequest;

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
    pub build: BuildSpecification,
    pub paths: Vec<PathManifestEntry>,
    pub outputs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpecification {
    pub derivation_path: Vec<u8>,
    pub outputs: Vec<NamedOutput>,
    pub input_sources: Vec<Vec<u8>>,
    pub system: String,
    pub required_system_features: Vec<String>,
    pub builder: Vec<u8>,
    pub arguments: Vec<Vec<u8>>,
    pub environment: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NamedOutput {
    pub name: Vec<u8>,
    pub path: Vec<u8>,
    pub hash_algorithm: Vec<u8>,
    pub hash: Vec<u8>,
}

impl InputManifest {
    pub fn validate(&self, maximum_paths: usize, maximum_nar_bytes: u64) -> io::Result<()> {
        validate_store_path(&self.derivation_path, true)?;
        self.build.validate(maximum_paths)?;
        if self.build.derivation_path != self.derivation_path.as_bytes() {
            return Err(invalid_data(
                "Nomad transfer build derivation does not match manifest",
            ));
        }
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
        let exact_outputs = self
            .build
            .outputs
            .iter()
            .map(|output| output.path.as_slice())
            .collect::<BTreeSet<_>>();
        let mut outputs = BTreeSet::new();
        for output in &self.outputs {
            validate_store_path(output, false)?;
            if !outputs.insert(output.as_str()) {
                return Err(invalid_data(
                    "Nomad transfer manifest contains duplicate output",
                ));
            }
        }
        if outputs
            .iter()
            .map(|output| output.as_bytes())
            .collect::<BTreeSet<_>>()
            != exact_outputs
        {
            return Err(invalid_data(
                "Nomad transfer build outputs do not match manifest",
            ));
        }
        let admitted = self
            .paths
            .iter()
            .map(|entry| entry.path.as_bytes())
            .collect::<BTreeSet<_>>();
        if self
            .build
            .input_sources
            .iter()
            .any(|input| !admitted.contains(input.as_slice()))
        {
            return Err(invalid_data(
                "Nomad transfer build input source is not admitted",
            ));
        }
        Ok(())
    }
}

impl From<&BuildRequest> for BuildSpecification {
    fn from(request: &BuildRequest) -> Self {
        Self {
            derivation_path: request.derivation_path().to_vec(),
            outputs: request
                .output_authorities()
                .iter()
                .map(|output| NamedOutput {
                    name: output.name().to_vec(),
                    path: output.path().to_vec(),
                    hash_algorithm: output.hash_algorithm().to_vec(),
                    hash: output.hash().to_vec(),
                })
                .collect(),
            input_sources: request.input_sources().to_vec(),
            system: request.system().to_owned(),
            required_system_features: request.required_system_features().to_vec(),
            builder: request.builder().to_vec(),
            arguments: request.arguments().to_vec(),
            environment: request.environment().to_vec(),
        }
    }
}

impl BuildSpecification {
    fn validate(&self, maximum_paths: usize) -> io::Result<()> {
        validate_store_path_bytes(&self.derivation_path, true)?;
        if self.outputs.is_empty()
            || self.outputs.len() > maximum_paths
            || self.input_sources.len() > maximum_paths
            || self.system.is_empty()
            || self.builder.is_empty()
            || self.arguments.len() > maximum_paths
            || self.environment.len() > maximum_paths
        {
            return Err(invalid_data(
                "Nomad transfer build specification exceeds limit",
            ));
        }
        let mut output_names = BTreeSet::new();
        let mut output_paths = BTreeSet::new();
        for output in &self.outputs {
            if output.name.is_empty()
                || !output_names.insert(output.name.as_slice())
                || !output_paths.insert(output.path.as_slice())
            {
                return Err(invalid_data("Nomad transfer build output is invalid"));
            }
            validate_store_path_bytes(&output.path, false)?;
            validate_output_authority(&output.hash_algorithm, &output.hash)?;
            if environment_value(&self.environment, &output.name) != Some(output.path.as_slice()) {
                return Err(invalid_data("Nomad transfer build output is inconsistent"));
            }
        }
        let mut inputs = BTreeSet::new();
        for input in &self.input_sources {
            validate_store_path_bytes(input, input.ends_with(b".drv"))?;
            if !inputs.insert(input.as_slice()) {
                return Err(invalid_data("Nomad transfer build input is duplicated"));
            }
        }
        if environment_value(&self.environment, b"system") != Some(self.system.as_bytes())
            || environment_value(&self.environment, b"builder") != Some(self.builder.as_slice())
            || self.environment.iter().any(|(name, _)| name.is_empty())
        {
            return Err(invalid_data(
                "Nomad transfer build specification is inconsistent",
            ));
        }
        Ok(())
    }
}

fn validate_output_authority(hash_algorithm: &[u8], hash: &[u8]) -> io::Result<()> {
    if hash_algorithm.is_empty() && hash.is_empty() {
        return Ok(());
    }
    let algorithm = hash_algorithm.strip_prefix(b"r:").unwrap_or(hash_algorithm);
    let expected_hex_bytes = match algorithm {
        b"md5" => 32,
        b"sha1" => 40,
        b"sha256" => 64,
        b"sha512" => 128,
        _ => {
            return Err(invalid_data(
                "Nomad transfer build output hash algorithm is invalid",
            ))
        }
    };
    if hash.len() != expected_hex_bytes
        || !hash
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_data("Nomad transfer build output hash is invalid"));
    }
    Ok(())
}

fn environment_value<'a>(environment: &'a [(Vec<u8>, Vec<u8>)], name: &[u8]) -> Option<&'a [u8]> {
    let mut values = environment
        .iter()
        .filter(|(candidate, _)| candidate.as_slice() == name)
        .map(|(_, value)| value.as_slice());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn validate_store_path_bytes(path: &[u8], derivation: bool) -> io::Result<()> {
    let path = std::str::from_utf8(path)
        .map_err(|_| invalid_data("Nomad transfer store path is invalid"))?;
    validate_store_path(path, derivation)
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
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_final_chunk")]
    pub final_chunk: bool,
}

const fn default_final_chunk() -> bool {
    true
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

#[derive(Debug)]
pub struct TransferSession {
    protocol: ProtocolSession,
    inputs: InputTransferSession,
    outputs: OutputTransferSession,
    derivation_path: String,
    maximum_log_chunk_bytes: usize,
    maximum_metadata_bytes: usize,
    next_log_sequence: u64,
}

impl TransferSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manifest: InputManifest,
        maximum_paths: usize,
        maximum_input_nar_bytes: u64,
        maximum_total_input_bytes: u64,
        maximum_output_nar_bytes: u64,
        maximum_total_output_bytes: u64,
        maximum_log_chunk_bytes: usize,
        maximum_metadata_bytes: usize,
    ) -> io::Result<Self> {
        if maximum_log_chunk_bytes == 0 || maximum_metadata_bytes == 0 {
            return Err(invalid_data(
                "Nomad transfer session configuration is invalid",
            ));
        }
        let derivation_path = manifest.derivation_path.clone();
        let outputs = OutputTransferSession::new(
            manifest.outputs.clone(),
            maximum_output_nar_bytes,
            maximum_total_output_bytes,
            maximum_metadata_bytes,
        )?;
        let inputs = InputTransferSession::new(
            manifest,
            maximum_paths,
            maximum_input_nar_bytes,
            maximum_total_input_bytes,
        )?;
        Ok(Self {
            protocol: ProtocolSession::resolving_inputs(),
            inputs,
            outputs,
            derivation_path,
            maximum_log_chunk_bytes,
            maximum_metadata_bytes,
            next_log_sequence: 0,
        })
    }

    pub fn accept(&mut self, direction: Direction, frame: Frame) -> io::Result<()> {
        match frame.kind() {
            FrameKind::ValidPaths => {
                require_empty_payload(&frame)?;
                self.inputs.record_valid_paths(decode_metadata(
                    frame.metadata(),
                    self.maximum_metadata_bytes,
                )?)?;
            }
            FrameKind::InputRequest => {
                require_empty_payload(&frame)?;
                let requested: PathSet =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                if requested != self.inputs.request_unresolved()? {
                    return Err(invalid_data(
                        "Nomad input request does not match unresolved paths",
                    ));
                }
            }
            FrameKind::InputNar => {
                let metadata: NarMetadata =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                self.inputs
                    .receive_nar_chunk(metadata, frame.payload().len() as u64)?;
            }
            FrameKind::BuildStarted => {
                require_empty_payload(&frame)?;
                let started: BuildStarted =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                if started.derivation_path != self.derivation_path {
                    return Err(invalid_data("Nomad build start derivation is invalid"));
                }
                self.inputs.ready_to_build()?;
            }
            FrameKind::LogChunk => {
                let metadata: LogChunk =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                if metadata.sequence != self.next_log_sequence
                    || frame.payload().len() > self.maximum_log_chunk_bytes
                {
                    return Err(invalid_data("Nomad live log chunk is invalid"));
                }
                self.next_log_sequence = self
                    .next_log_sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("Nomad live log sequence is invalid"))?;
            }
            FrameKind::OutputMetadata => {
                require_empty_payload(&frame)?;
                self.outputs.declare(decode_metadata(
                    frame.metadata(),
                    self.maximum_metadata_bytes,
                )?)?;
            }
            FrameKind::OutputNar => {
                let metadata: NarMetadata =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                self.outputs.receive_nar_chunk(
                    &metadata.path,
                    metadata.offset,
                    frame.payload().len() as u64,
                    metadata.final_chunk,
                )?;
            }
            FrameKind::OutputReceipt => {
                require_empty_payload(&frame)?;
                self.outputs.record_receipt(decode_metadata(
                    frame.metadata(),
                    self.maximum_metadata_bytes,
                )?)?;
            }
            FrameKind::BuildResult => {
                require_empty_payload(&frame)?;
                let result: BuildResultMetadata =
                    decode_metadata(frame.metadata(), self.maximum_metadata_bytes)?;
                self.outputs.finish(&result)?;
            }
            FrameKind::Authenticate | FrameKind::InputManifest => {
                return Err(invalid_data(
                    "Nomad transfer frame is invalid for active session",
                ));
            }
        }
        self.protocol.accept(direction, frame.kind())
    }

    pub fn is_complete(&self) -> bool {
        self.protocol.is_complete() && self.outputs.is_complete()
    }
}

fn require_empty_payload(frame: &Frame) -> io::Result<()> {
    if !frame.payload().is_empty() {
        return Err(invalid_data("Nomad transfer frame payload is invalid"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputState {
    AwaitingResolution,
    Available,
    Requested { received_bytes: u64 },
    Received,
}

#[derive(Debug)]
pub struct InputTransferSession {
    manifest: InputManifest,
    states: std::collections::BTreeMap<String, InputState>,
    maximum_paths: usize,
    maximum_nar_bytes: u64,
    maximum_total_bytes: u64,
    resolution_recorded: bool,
    request_created: bool,
}

impl InputTransferSession {
    pub fn new(
        manifest: InputManifest,
        maximum_paths: usize,
        maximum_nar_bytes: u64,
        maximum_total_bytes: u64,
    ) -> io::Result<Self> {
        if maximum_paths == 0 || maximum_nar_bytes == 0 || maximum_total_bytes == 0 {
            return Err(invalid_data(
                "Nomad input transfer configuration is invalid",
            ));
        }
        manifest.validate(maximum_paths, maximum_nar_bytes)?;
        let states = manifest
            .paths
            .iter()
            .map(|entry| (entry.path.clone(), InputState::AwaitingResolution))
            .collect();
        Ok(Self {
            manifest,
            states,
            maximum_paths,
            maximum_nar_bytes,
            maximum_total_bytes,
            resolution_recorded: false,
            request_created: false,
        })
    }

    pub fn record_valid_paths(&mut self, paths: PathSet) -> io::Result<()> {
        if self.resolution_recorded || self.request_created {
            return Err(invalid_data("Nomad input resolution is duplicated"));
        }
        let admitted = self.states.keys().cloned().collect::<Vec<_>>();
        paths.validate_against(&admitted, self.maximum_paths)?;
        for path in paths.paths {
            let state = self
                .states
                .get_mut(&path)
                .ok_or_else(|| invalid_data("Nomad input path is not admitted"))?;
            *state = InputState::Available;
        }
        self.resolution_recorded = true;
        Ok(())
    }

    pub fn request_unresolved(&mut self) -> io::Result<PathSet> {
        if !self.resolution_recorded || self.request_created {
            return Err(invalid_data("Nomad input request is out of order"));
        }
        let unresolved = self
            .manifest
            .paths
            .iter()
            .filter(|entry| self.states.get(&entry.path) == Some(&InputState::AwaitingResolution))
            .collect::<Vec<_>>();
        unresolved.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(entry.nar_size)
                .filter(|value| *value <= self.maximum_total_bytes)
                .ok_or_else(|| invalid_data("Nomad input transfer exceeds total byte limit"))
        })?;
        let paths = unresolved
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        for path in &paths {
            self.states
                .insert(path.clone(), InputState::Requested { received_bytes: 0 });
        }
        self.request_created = true;
        Ok(PathSet { paths })
    }

    pub fn receive_nar_chunk(
        &mut self,
        metadata: NarMetadata,
        received_bytes: u64,
    ) -> io::Result<()> {
        if !self.request_created {
            return Err(invalid_data("Nomad input NAR is out of order"));
        }
        let entry = self
            .manifest
            .paths
            .iter()
            .find(|entry| entry.path == metadata.path)
            .ok_or_else(|| invalid_data("Nomad input path is not admitted"))?;
        metadata.validate_against(
            &self.states.keys().cloned().collect::<Vec<_>>(),
            self.maximum_nar_bytes,
        )?;
        let Some(InputState::Requested {
            received_bytes: prior_bytes,
        }) = self.states.get(&metadata.path).copied()
        else {
            return Err(invalid_data("Nomad input NAR does not match manifest"));
        };
        let total_bytes = prior_bytes
            .checked_add(received_bytes)
            .ok_or_else(|| invalid_data("Nomad input NAR length is invalid"))?;
        if metadata.nar_hash != entry.nar_hash
            || metadata.nar_size != entry.nar_size
            || metadata.offset != prior_bytes
            || received_bytes == 0
            || total_bytes > entry.nar_size
            || metadata.final_chunk != (total_bytes == entry.nar_size)
        {
            return Err(invalid_data("Nomad input NAR does not match manifest"));
        }
        self.states.insert(
            metadata.path,
            if metadata.final_chunk {
                InputState::Received
            } else {
                InputState::Requested {
                    received_bytes: total_bytes,
                }
            },
        );
        Ok(())
    }

    pub fn ready_to_build(&self) -> io::Result<()> {
        if !self.resolution_recorded
            || !self.request_created
            || !self
                .states
                .values()
                .all(|state| matches!(state, InputState::Available | InputState::Received))
        {
            return Err(invalid_data("Nomad inputs are unresolved"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputState {
    Expected,
    Declared { nar_size: u64, received_bytes: u64 },
    Received,
    Accepted,
}

#[derive(Debug)]
pub struct OutputTransferSession {
    outputs: std::collections::BTreeMap<String, OutputState>,
    maximum_nar_bytes: u64,
    maximum_total_bytes: u64,
    maximum_diagnostic_bytes: usize,
    declared_total_bytes: u64,
    complete: bool,
}

impl OutputTransferSession {
    pub fn new(
        expected_outputs: Vec<String>,
        maximum_nar_bytes: u64,
        maximum_total_bytes: u64,
        maximum_diagnostic_bytes: usize,
    ) -> io::Result<Self> {
        if expected_outputs.is_empty()
            || maximum_nar_bytes == 0
            || maximum_total_bytes == 0
            || maximum_diagnostic_bytes == 0
        {
            return Err(invalid_data(
                "Nomad output transfer configuration is invalid",
            ));
        }
        let mut outputs = std::collections::BTreeMap::new();
        for output in expected_outputs {
            validate_store_path(&output, false)?;
            if outputs.insert(output, OutputState::Expected).is_some() {
                return Err(invalid_data(
                    "Nomad output transfer contains duplicate output",
                ));
            }
        }
        Ok(Self {
            outputs,
            maximum_nar_bytes,
            maximum_total_bytes,
            maximum_diagnostic_bytes,
            declared_total_bytes: 0,
            complete: false,
        })
    }

    pub fn declare(&mut self, metadata: PathManifestEntry) -> io::Result<()> {
        if self.complete {
            return Err(invalid_data("Nomad output transfer is complete"));
        }
        metadata.validate(self.outputs.len(), self.maximum_nar_bytes)?;
        let state = self
            .outputs
            .get_mut(&metadata.path)
            .ok_or_else(|| invalid_data("Nomad output is not expected"))?;
        if *state != OutputState::Expected {
            return Err(invalid_data("Nomad output metadata is duplicated"));
        }
        let declared_total_bytes = self
            .declared_total_bytes
            .checked_add(metadata.nar_size)
            .filter(|total| *total <= self.maximum_total_bytes)
            .ok_or_else(|| invalid_data("Nomad output transfer exceeds total byte limit"))?;
        *state = OutputState::Declared {
            nar_size: metadata.nar_size,
            received_bytes: 0,
        };
        self.declared_total_bytes = declared_total_bytes;
        Ok(())
    }

    pub fn receive_nar_chunk(
        &mut self,
        path: &str,
        offset: u64,
        received_bytes: u64,
        final_chunk: bool,
    ) -> io::Result<()> {
        if self.complete {
            return Err(invalid_data("Nomad output transfer is complete"));
        }
        let state = self
            .outputs
            .get_mut(path)
            .ok_or_else(|| invalid_data("Nomad output is not expected"))?;
        match *state {
            OutputState::Declared {
                nar_size,
                received_bytes: prior_bytes,
            } => {
                let total_bytes = prior_bytes
                    .checked_add(received_bytes)
                    .ok_or_else(|| invalid_data("Nomad output NAR length is invalid"))?;
                if offset != prior_bytes
                    || received_bytes == 0
                    || total_bytes > nar_size
                    || final_chunk != (total_bytes == nar_size)
                {
                    return Err(invalid_data("Nomad output NAR does not match declaration"));
                }
                *state = if final_chunk {
                    OutputState::Received
                } else {
                    OutputState::Declared {
                        nar_size,
                        received_bytes: total_bytes,
                    }
                };
                Ok(())
            }
            _ => Err(invalid_data("Nomad output NAR does not match declaration")),
        }
    }

    pub fn record_receipt(&mut self, receipt: OutputReceipt) -> io::Result<()> {
        if self.complete || !receipt.accepted {
            return Err(invalid_data("Nomad output receipt is invalid"));
        }
        let state = self
            .outputs
            .get_mut(&receipt.path)
            .ok_or_else(|| invalid_data("Nomad output is not expected"))?;
        if *state != OutputState::Received {
            return Err(invalid_data("Nomad output receipt is out of order"));
        }
        *state = OutputState::Accepted;
        Ok(())
    }

    pub fn finish(&mut self, result: &BuildResultMetadata) -> io::Result<()> {
        if self.complete {
            return Err(invalid_data("Nomad output transfer is complete"));
        }
        if result
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.len() > self.maximum_diagnostic_bytes)
        {
            return Err(invalid_data("Nomad build diagnostic exceeds limit"));
        }
        match result.outcome {
            BuildOutcome::Built
                if result.diagnostic.is_none()
                    && self
                        .outputs
                        .values()
                        .all(|state| *state == OutputState::Accepted) => {}
            BuildOutcome::Failed => {}
            _ => return Err(invalid_data("Nomad terminal build result is invalid")),
        }
        self.complete = true;
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }
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

    const fn resolving_inputs() -> Self {
        Self {
            phase: Phase::ResolvingInputs,
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
                ));
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
