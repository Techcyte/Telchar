//! Runs the allocation-side authenticated transfer session, resolves inputs, invokes Nix, and returns logs and outputs.

use std::io;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use telchar::nomad::protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, Authentication, AuthenticationProof,
    BuildOutcome, BuildResultMetadata, BuildStarted, Direction, Frame, FrameKind, InputManifest,
    InputTransferSession, LogChunk, NarMetadata, OutputReceipt, PathManifestEntry, PathSet,
    ProtocolLimits, ProtocolSession,
};
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};
use tungstenite::client::IntoClientRequest;
use url::Url;

const MAXIMUM_ENVIRONMENT_VALUE_BYTES: usize = 4096;
const MAXIMUM_AUTHENTICATION_METADATA_BYTES: usize = 16 * 1024;
const MAXIMUM_MANIFEST_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_MANIFEST_PATHS: usize = nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES;
const MAXIMUM_INPUT_NAR_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAXIMUM_NAR_CHUNK_BYTES: usize = 1024 * 1024;

type WorkerSocket =
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

pub struct WorkerSession {
    socket: WorkerSocket,
    manifest: InputManifest,
    protocol: ProtocolSession,
    inputs: InputTransferSession,
    transfer_chunk_bytes: usize,
    connection_deadline: Instant,
}

impl WorkerSession {
    pub fn manifest(&self) -> &InputManifest {
        &self.manifest
    }

    pub fn into_parts(self) -> (WorkerSocket, InputManifest) {
        (self.socket, self.manifest)
    }

    pub fn resolve_inputs(&mut self, store_uri: &str) -> io::Result<PathSet> {
        self.ensure_connection_active()?;
        let endpoint = GatewayStoreEndpoint::parse(store_uri)
            .map_err(|_| invalid("worker Nix store URI is invalid"))?;
        let mut store = GatewayStoreConnection::connect(&endpoint)
            .map_err(|_| io::Error::other("worker Nix store connection failed"))?;
        let mut valid = Vec::new();
        for entry in &self.manifest.paths {
            if store
                .is_valid_path(entry.path.as_bytes())
                .map_err(|_| io::Error::other("worker Nix store query failed"))?
            {
                valid.push(entry.path.clone());
            }
        }
        let valid = PathSet { paths: valid };
        self.inputs.record_valid_paths(valid.clone())?;
        self.send_metadata(FrameKind::ValidPaths, &valid)?;
        let available = valid
            .paths
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let unresolved = PathSet {
            paths: self
                .manifest
                .paths
                .iter()
                .map(|entry| &entry.path)
                .filter(|path| !available.contains(path))
                .cloned()
                .collect(),
        };
        let requested = requested_inputs(&self.manifest, unresolved)?;
        if requested
            .paths
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            != self
                .inputs
                .request_unresolved()?
                .paths
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        {
            return Err(invalid("worker unresolved input set is inconsistent"));
        }
        self.send_metadata(FrameKind::InputRequest, &requested)?;
        Ok(requested)
    }

    pub fn build(&mut self, store_uri: &str) -> io::Result<nix_worker_protocol::WorkerBuildResult> {
        self.ensure_connection_active()?;
        let endpoint = GatewayStoreEndpoint::parse(store_uri)
            .map_err(|_| invalid("worker Nix store URI is invalid"))?;
        let mut store = GatewayStoreConnection::connect(&endpoint)
            .map_err(|_| io::Error::other("worker Nix store connection failed"))?;
        let specification = self.manifest.build.clone();
        let derivation_path = self.manifest.derivation_path.clone();
        self.send_metadata(FrameKind::BuildStarted, &BuildStarted { derivation_path })?;
        let outputs = specification
            .outputs
            .iter()
            .map(|output| nix_worker_protocol::BuildDerivationOutputRequest {
                name: &output.name,
                path: &output.path,
                hash_algorithm: &output.hash_algorithm,
                hash: &output.hash,
            })
            .collect::<Vec<_>>();
        let request = nix_worker_protocol::BuildDerivationClientRequest {
            drv_path: &specification.derivation_path,
            outputs: &outputs,
            input_sources: &specification.input_sources,
            platform: specification.system.as_bytes(),
            builder: &specification.builder,
            arguments: &specification.arguments,
            environment: &specification.environment,
        };
        let socket = &mut self.socket;
        let protocol = &mut self.protocol;
        let mut sequence = 0_u64;
        store.build_derivation(&request, &mut |message| {
            for chunk in message.chunks(MAXIMUM_AUTHENTICATION_METADATA_BYTES) {
                let frame = Frame::new(
                    FrameKind::LogChunk,
                    encode_metadata(&LogChunk { sequence }, MAXIMUM_MANIFEST_METADATA_BYTES)?,
                    chunk.to_vec(),
                );
                protocol.accept(Direction::WorkerToGateway, FrameKind::LogChunk)?;
                let mut body = Vec::new();
                write_frame(
                    &mut body,
                    &frame,
                    ProtocolLimits::new(MAXIMUM_MANIFEST_METADATA_BYTES, chunk.len()),
                )?;
                socket
                    .send(tungstenite::Message::Binary(body.into()))
                    .map_err(|_| io::Error::other("worker build log send failed"))?;
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("worker build log sequence overflow"))?;
            }
            Ok(())
        })
    }

    pub fn report_failure(&mut self, error: &io::Error, maximum_bytes: usize) -> io::Result<()> {
        let diagnostic = error.to_string();
        let diagnostic = diagnostic
            .char_indices()
            .take_while(|(offset, _)| *offset < maximum_bytes)
            .map(|(_, character)| character)
            .collect::<String>();
        self.send_metadata(
            FrameKind::BuildResult,
            &BuildResultMetadata {
                outcome: BuildOutcome::Failed,
                diagnostic: Some(diagnostic),
            },
        )
    }

    pub fn return_outputs(
        &mut self,
        store_uri: &str,
        result: &nix_worker_protocol::WorkerBuildResult,
    ) -> io::Result<()> {
        self.ensure_connection_active()?;
        let mut actual_outputs = result.outputs().to_vec();
        actual_outputs.sort();
        let mut expected_outputs = self
            .manifest
            .build
            .outputs
            .iter()
            .map(|output| (output.name.clone(), output.path.clone()))
            .collect::<Vec<_>>();
        expected_outputs.sort();
        if actual_outputs != expected_outputs {
            return Err(invalid("worker build output set is inconsistent"));
        }
        let endpoint = GatewayStoreEndpoint::parse(store_uri)
            .map_err(|_| invalid("worker Nix store URI is invalid"))?;
        let mut store = GatewayStoreConnection::connect(&endpoint)
            .map_err(|_| io::Error::other("worker Nix store connection failed"))?;
        for (_, path) in expected_outputs {
            self.ensure_connection_active()?;
            let info = store
                .query_path_info(&path)
                .map_err(|_| io::Error::other("worker output metadata query failed"))?
                .ok_or_else(|| invalid("worker build output is unavailable"))?;
            let metadata = PathManifestEntry {
                path: String::from_utf8(path.clone())
                    .map_err(|_| invalid("worker build output path is invalid"))?,
                nar_hash: info.nar_hash_hex().to_owned(),
                nar_size: info.nar_size(),
                references: info
                    .references()
                    .iter()
                    .map(|reference| {
                        String::from_utf8(reference.clone())
                            .map_err(|_| invalid("worker output reference is invalid"))
                    })
                    .collect::<io::Result<Vec<_>>>()?,
                deriver: info
                    .deriver()
                    .map(|deriver| {
                        String::from_utf8(deriver.to_vec())
                            .map_err(|_| invalid("worker output deriver is invalid"))
                    })
                    .transpose()?,
                content_address: info
                    .content_address()
                    .map(|address| {
                        String::from_utf8(address.to_vec())
                            .map_err(|_| invalid("worker output content address is invalid"))
                    })
                    .transpose()?,
            };
            self.send_metadata(FrameKind::OutputMetadata, &metadata)?;
            let mut sink = OutputNarWriter::new(
                &mut self.socket,
                &mut self.protocol,
                metadata.clone(),
                self.transfer_chunk_bytes,
            );
            store
                .nar_from_path(&path, metadata.nar_size, &mut sink)
                .map_err(|_| io::Error::other("worker output export failed"))?;
            sink.finish()?;
            let receipt = self.read_metadata::<OutputReceipt>(FrameKind::OutputReceipt)?;
            if receipt.path != metadata.path || !receipt.accepted {
                return Err(invalid("gateway rejected worker output"));
            }
        }
        self.send_metadata(
            FrameKind::BuildResult,
            &BuildResultMetadata {
                outcome: BuildOutcome::Built,
                diagnostic: None,
            },
        )
    }

    pub fn import_requested_inputs(
        &mut self,
        store_uri: &str,
        requested: &PathSet,
    ) -> io::Result<()> {
        self.ensure_connection_active()?;
        let endpoint = GatewayStoreEndpoint::parse(store_uri)
            .map_err(|_| invalid("worker Nix store URI is invalid"))?;
        let mut store = GatewayStoreConnection::connect(&endpoint)
            .map_err(|_| io::Error::other("worker Nix store connection failed"))?;
        if requested_inputs(&self.manifest, requested.clone())? != *requested {
            return Err(invalid("worker input request order is inconsistent"));
        }
        for path in &requested.paths {
            self.ensure_connection_active()?;
            let entry = self
                .manifest
                .paths
                .iter()
                .find(|entry| &entry.path == path)
                .ok_or_else(|| invalid("worker requested input is not admitted"))?;
            let references = entry
                .references
                .iter()
                .map(|reference| reference.as_bytes().to_vec())
                .collect::<Vec<_>>();
            let info = nix_worker_protocol::AddToStoreNarInfo {
                path: entry.path.as_bytes(),
                deriver: entry.deriver.as_deref().map(str::as_bytes),
                nar_hash_hex: &entry.nar_hash,
                references: &references,
                registration_time: 0,
                nar_size: entry.nar_size,
                ultimate: false,
                signatures: &[],
                content_address: None,
            };
            eprintln!(
                "telchar-nomad-worker: importing input path={} nar_size={} reference_count={}",
                entry.path,
                entry.nar_size,
                entry.references.len()
            );
            let mut source = InputNarReader::new(
                &mut self.socket,
                &mut self.protocol,
                &mut self.inputs,
                entry,
            );
            if let Err(error) = store.add_to_store_nar(&info, &mut source, false, true) {
                eprintln!(
                    "telchar-nomad-worker: input import reader failed path={} stage={:?}",
                    entry.path,
                    source.failure_stage()
                );
                return Err(io::Error::other(format!(
                    "worker input import failed for {}: {error}",
                    entry.path
                )));
            }
            source.finish()?;
            eprintln!("telchar-nomad-worker: imported input path={}", entry.path);
        }
        self.inputs.ready_to_build()
    }

    fn ensure_connection_active(&self) -> io::Result<()> {
        if Instant::now() >= self.connection_deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "worker connection lifetime exceeded",
            ));
        }
        Ok(())
    }

    fn read_metadata<T: serde::de::DeserializeOwned>(&mut self, kind: FrameKind) -> io::Result<T> {
        self.ensure_connection_active()?;
        let body = read_binary_message(&mut self.socket)?;
        let mut input = body.as_slice();
        let frame = read_frame(
            &mut input,
            ProtocolLimits::new(MAXIMUM_MANIFEST_METADATA_BYTES, 0),
        )?;
        if !input.is_empty() || frame.kind() != kind || !frame.payload().is_empty() {
            return Err(invalid("worker received invalid transfer metadata"));
        }
        self.protocol.accept(Direction::GatewayToWorker, kind)?;
        decode_metadata(frame.metadata(), MAXIMUM_MANIFEST_METADATA_BYTES)
    }

    fn send_metadata<T: serde::Serialize>(&mut self, kind: FrameKind, value: &T) -> io::Result<()> {
        let metadata = encode_metadata(value, MAXIMUM_MANIFEST_METADATA_BYTES)?;
        let frame = Frame::new(kind, metadata, Vec::new());
        self.protocol.accept(Direction::WorkerToGateway, kind)?;
        let mut body = Vec::new();
        write_frame(
            &mut body,
            &frame,
            ProtocolLimits::new(MAXIMUM_MANIFEST_METADATA_BYTES, 0),
        )?;
        self.socket
            .send(tungstenite::Message::Binary(body.into()))
            .map_err(|_| io::Error::other("worker input resolution send failed"))
    }
}

struct OutputNarWriter<'a> {
    socket: &'a mut WorkerSocket,
    protocol: &'a mut ProtocolSession,
    metadata: PathManifestEntry,
    offset: u64,
    buffered: Vec<u8>,
    chunk_bytes: usize,
}

impl<'a> OutputNarWriter<'a> {
    fn new(
        socket: &'a mut WorkerSocket,
        protocol: &'a mut ProtocolSession,
        metadata: PathManifestEntry,
        chunk_bytes: usize,
    ) -> Self {
        Self {
            socket,
            protocol,
            metadata,
            offset: 0,
            buffered: Vec::with_capacity(chunk_bytes),
            chunk_bytes,
        }
    }

    fn send_chunk(&mut self, final_chunk: bool) -> io::Result<()> {
        if self.buffered.is_empty() {
            return Err(invalid("worker output NAR chunk is empty"));
        }
        let metadata = NarMetadata {
            path: self.metadata.path.clone(),
            nar_hash: self.metadata.nar_hash.clone(),
            nar_size: self.metadata.nar_size,
            offset: self.offset,
            final_chunk,
        };
        let payload = std::mem::take(&mut self.buffered);
        let frame = Frame::new(
            FrameKind::OutputNar,
            encode_metadata(&metadata, MAXIMUM_MANIFEST_METADATA_BYTES)?,
            payload,
        );
        self.protocol
            .accept(Direction::WorkerToGateway, FrameKind::OutputNar)?;
        let mut body = Vec::new();
        write_frame(
            &mut body,
            &frame,
            ProtocolLimits::new(MAXIMUM_MANIFEST_METADATA_BYTES, self.chunk_bytes),
        )?;
        self.socket
            .send(tungstenite::Message::Binary(body.into()))
            .map_err(|_| io::Error::other("worker output NAR send failed"))?;
        self.offset = self
            .offset
            .checked_add(frame.payload().len() as u64)
            .ok_or_else(|| invalid("worker output NAR offset overflow"))?;
        self.buffered = Vec::with_capacity(self.chunk_bytes);
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.offset + self.buffered.len() as u64 != self.metadata.nar_size {
            return Err(invalid("worker output NAR length is inconsistent"));
        }
        self.send_chunk(true)
    }
}

impl io::Write for OutputNarWriter<'_> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let received = input.len();
        while !input.is_empty() {
            let available = self.chunk_bytes - self.buffered.len();
            let copied = available.min(input.len());
            self.buffered.extend_from_slice(&input[..copied]);
            input = &input[copied..];
            if self.buffered.len() == self.chunk_bytes
                && self.offset + (self.buffered.len() as u64) < self.metadata.nar_size
            {
                self.send_chunk(false)?;
            }
        }
        Ok(received)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    endpoint: Url,
    store_uri: String,
    transfer_chunk_bytes: usize,
    maximum_manifest_bytes: usize,
    transfer_idle_timeout: std::time::Duration,
    output_collection_timeout: std::time::Duration,
    maximum_connection_lifetime: std::time::Duration,
    maximum_diagnostic_bytes: usize,
    authentication: Authentication,
}

impl WorkerConfig {
    pub fn from_environment() -> io::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> io::Result<Self> {
        let endpoint = required(&mut lookup, "TELCHAR_TRANSFER_ENDPOINT")?;
        let endpoint =
            Url::parse(&endpoint).map_err(|_| invalid("worker callback endpoint is invalid"))?;
        if !matches!(endpoint.scheme(), "ws" | "wss")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid("worker callback endpoint is invalid"));
        }
        let store_uri = required(&mut lookup, "TELCHAR_NIX_STORE_URI")?;
        let transfer_chunk_bytes = required(&mut lookup, "TELCHAR_TRANSFER_CHUNK_BYTES")?
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0 && *value <= MAXIMUM_NAR_CHUNK_BYTES)
            .ok_or_else(|| invalid("worker transfer chunk limit is invalid"))?;
        let maximum_manifest_bytes = required(&mut lookup, "TELCHAR_MAXIMUM_MANIFEST_BYTES")?
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0 && *value <= MAXIMUM_MANIFEST_METADATA_BYTES)
            .ok_or_else(|| invalid("worker manifest limit is invalid"))?;
        let transfer_idle_timeout = required(&mut lookup, "TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS")?
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .map(std::time::Duration::from_secs)
            .ok_or_else(|| invalid("worker transfer idle timeout is invalid"))?;
        let output_collection_timeout =
            required(&mut lookup, "TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS")?
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .map(std::time::Duration::from_secs)
                .ok_or_else(|| invalid("worker output collection timeout is invalid"))?;
        let maximum_connection_lifetime =
            required(&mut lookup, "TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS")?
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .map(std::time::Duration::from_secs)
                .ok_or_else(|| invalid("worker connection lifetime is invalid"))?;
        let maximum_diagnostic_bytes = required(&mut lookup, "TELCHAR_MAXIMUM_DIAGNOSTIC_BYTES")?
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0 && *value <= MAXIMUM_MANIFEST_METADATA_BYTES)
            .ok_or_else(|| invalid("worker diagnostic limit is invalid"))?;
        let mode = required(&mut lookup, "TELCHAR_TRANSFER_AUTHENTICATION")?;
        let allocation_id = required(&mut lookup, "NOMAD_ALLOC_ID")?;
        let nomad_namespace = required(&mut lookup, "NOMAD_NAMESPACE")?;
        let nomad_job_id = required(&mut lookup, "NOMAD_JOB_ID")?;
        let nomad_task = required(&mut lookup, "NOMAD_TASK_NAME")?;

        let (backend, namespace, job_id, shared_build_digest, task, proof) = match mode.as_str() {
            "workload-identity" => {
                let namespace = required(&mut lookup, "TELCHAR_NAMESPACE")?;
                let job_id = required(&mut lookup, "TELCHAR_JOB_ID")?;
                let task = required(&mut lookup, "TELCHAR_TASK")?;
                if namespace != nomad_namespace || job_id != nomad_job_id || task != nomad_task {
                    return Err(invalid(
                        "worker Nomad identity does not match submitted job",
                    ));
                }
                (
                    required(&mut lookup, "TELCHAR_BACKEND")?,
                    namespace,
                    job_id,
                    required(&mut lookup, "TELCHAR_SHARED_BUILD_DIGEST")?,
                    task,
                    AuthenticationProof::WorkloadIdentity {
                        token: required(&mut lookup, "NOMAD_TOKEN_telchar_transfer")?,
                    },
                )
            }
            "hmac" => {
                let capability = required(&mut lookup, "TELCHAR_TRANSFER_CAPABILITY")?;
                let claims = decode_capability(&capability)?;
                if claims.namespace != nomad_namespace
                    || claims.job_id != nomad_job_id
                    || nomad_task != "build"
                {
                    return Err(invalid(
                        "worker Nomad identity does not match signed capability",
                    ));
                }
                let expiry = claims.expires_at;
                let nonce = claims.nonce.clone();
                let body_digest = authentication_body_digest(
                    &claims.backend,
                    &claims.namespace,
                    &claims.job_id,
                    &allocation_id,
                    &nomad_task,
                    &claims.shared_build_digest,
                    &capability,
                    expiry,
                    &nonce,
                )?;
                let signature = request_signature(
                    &claims.request_key,
                    &capability,
                    endpoint.path(),
                    &body_digest,
                    expiry,
                    &nonce,
                )?;
                (
                    claims.backend,
                    claims.namespace,
                    claims.job_id,
                    claims.shared_build_digest,
                    nomad_task,
                    AuthenticationProof::Hmac {
                        capability,
                        expiry,
                        nonce,
                        body_digest,
                        signature,
                    },
                )
            }
            _ => return Err(invalid("worker transfer authentication mode is invalid")),
        };

        Ok(Self {
            endpoint,
            store_uri,
            transfer_chunk_bytes,
            maximum_manifest_bytes,
            transfer_idle_timeout,
            output_collection_timeout,
            maximum_connection_lifetime,
            maximum_diagnostic_bytes,
            authentication: Authentication {
                backend,
                namespace,
                job_id,
                allocation_id,
                task,
                shared_build_digest,
                proof,
            },
        })
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn store_uri(&self) -> &str {
        &self.store_uri
    }

    pub fn transfer_chunk_bytes(&self) -> usize {
        self.transfer_chunk_bytes
    }

    pub fn maximum_manifest_bytes(&self) -> usize {
        self.maximum_manifest_bytes
    }

    pub fn transfer_idle_timeout(&self) -> std::time::Duration {
        self.transfer_idle_timeout
    }

    pub fn output_collection_timeout(&self) -> std::time::Duration {
        self.output_collection_timeout
    }

    pub fn maximum_connection_lifetime(&self) -> std::time::Duration {
        self.maximum_connection_lifetime
    }

    pub fn maximum_diagnostic_bytes(&self) -> usize {
        self.maximum_diagnostic_bytes
    }

    pub fn authentication(&self) -> &Authentication {
        &self.authentication
    }
}

pub fn connect(config: &WorkerConfig) -> io::Result<WorkerSocket> {
    let metadata = encode_metadata(
        config.authentication(),
        MAXIMUM_AUTHENTICATION_METADATA_BYTES,
    )?;
    let mut body = Vec::with_capacity(metadata.len() + 16);
    write_frame(
        &mut body,
        &Frame::new(FrameKind::Authenticate, metadata, Vec::new()),
        ProtocolLimits::new(MAXIMUM_AUTHENTICATION_METADATA_BYTES, 0),
    )?;
    let mut request = config
        .endpoint()
        .as_str()
        .into_client_request()
        .map_err(|_| io::Error::other("worker callback request could not be created"))?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        tungstenite::http::HeaderValue::from_static("telchar-nomad-transfer-v1"),
    );
    let (mut socket, response) = tungstenite::client::connect_with_config(
        request,
        Some(
            tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(config.maximum_manifest_bytes()))
                .max_frame_size(Some(config.maximum_manifest_bytes())),
        ),
        3,
    )
    .map_err(|_| io::Error::other("worker callback connection failed"))?;
    set_socket_timeouts(&mut socket, config.transfer_idle_timeout())?;
    if response.headers().get("sec-websocket-protocol")
        != Some(&tungstenite::http::HeaderValue::from_static(
            "telchar-nomad-transfer-v1",
        ))
    {
        return Err(io::Error::other("worker callback protocol was rejected"));
    }
    socket
        .send(tungstenite::Message::Binary(body.into()))
        .map_err(|_| io::Error::other("worker callback authentication failed"))?;
    Ok(socket)
}

fn set_socket_timeouts(socket: &mut WorkerSocket, timeout: std::time::Duration) -> io::Result<()> {
    match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))
        }
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
            stream.get_mut().set_read_timeout(Some(timeout))?;
            stream.get_mut().set_write_timeout(Some(timeout))
        }
        _ => Err(io::Error::other(
            "worker callback transport does not support timeouts",
        )),
    }
}

fn read_binary_message(socket: &mut WorkerSocket) -> io::Result<Vec<u8>> {
    loop {
        match socket
            .read()
            .map_err(|error| io::Error::other(format!("worker transfer receive failed: {error}")))?
        {
            tungstenite::Message::Binary(body) => return Ok(body.to_vec()),
            tungstenite::Message::Ping(payload) => socket
                .send(tungstenite::Message::Pong(payload))
                .map_err(|error| {
                    io::Error::other(format!("worker transfer response failed: {error}"))
                })?,
            tungstenite::Message::Close(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "worker callback closed during transfer",
                ));
            }
            tungstenite::Message::Text(_)
            | tungstenite::Message::Pong(_)
            | tungstenite::Message::Frame(_) => {
                return Err(invalid("worker transfer message is invalid"));
            }
        }
    }
}

pub fn receive_manifest(config: &WorkerConfig) -> io::Result<WorkerSession> {
    let mut socket = connect(config).map_err(|error| startup_error("connect", error))?;
    let message =
        read_binary_message(&mut socket).map_err(|error| startup_error("receive", error))?;
    let mut input = message.as_slice();
    let frame = read_frame(
        &mut input,
        ProtocolLimits::new(config.maximum_manifest_bytes(), 0),
    )
    .map_err(|error| startup_error("frame", error))?;
    if !input.is_empty() || frame.kind() != FrameKind::InputManifest || !frame.payload().is_empty()
    {
        return Err(startup_error(
            "frame-shape",
            invalid("worker manifest frame is invalid"),
        ));
    }
    let manifest: InputManifest =
        decode_metadata(frame.metadata(), config.maximum_manifest_bytes())
            .map_err(|error| startup_error("metadata", error))?;
    manifest
        .validate(MAXIMUM_MANIFEST_PATHS, MAXIMUM_INPUT_NAR_BYTES)
        .map_err(|error| startup_error("validation", error))?;
    let inputs = InputTransferSession::new(
        manifest.clone(),
        MAXIMUM_MANIFEST_PATHS,
        MAXIMUM_INPUT_NAR_BYTES,
        MAXIMUM_INPUT_NAR_BYTES
            .checked_mul(MAXIMUM_MANIFEST_PATHS as u64)
            .ok_or_else(|| invalid("worker manifest limit is invalid"))?,
    )
    .map_err(|error| startup_error("input-session", error))?;
    let mut protocol = ProtocolSession::new();
    protocol
        .accept(Direction::WorkerToGateway, FrameKind::Authenticate)
        .map_err(|error| startup_error("authentication-phase", error))?;
    protocol
        .accept(Direction::GatewayToWorker, FrameKind::InputManifest)
        .map_err(|error| startup_error("manifest-phase", error))?;
    Ok(WorkerSession {
        socket,
        manifest,
        protocol,
        inputs,
        transfer_chunk_bytes: config.transfer_chunk_bytes(),
        connection_deadline: Instant::now()
            .checked_add(config.maximum_connection_lifetime())
            .ok_or_else(|| invalid("worker connection lifetime is invalid"))?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputNarFailureStage {
    Receive,
    KeepaliveResponse,
    Message,
    Frame,
    Metadata,
    Path,
    Transfer,
    Protocol,
}

struct InputNarReader<'a> {
    socket: &'a mut WorkerSocket,
    protocol: &'a mut ProtocolSession,
    inputs: &'a mut InputTransferSession,
    entry: &'a telchar::nomad::protocol::PathManifestEntry,
    chunk: std::io::Cursor<Vec<u8>>,
    complete: bool,
    failure_stage: Option<InputNarFailureStage>,
}

impl<'a> InputNarReader<'a> {
    fn new(
        socket: &'a mut WorkerSocket,
        protocol: &'a mut ProtocolSession,
        inputs: &'a mut InputTransferSession,
        entry: &'a telchar::nomad::protocol::PathManifestEntry,
    ) -> Self {
        Self {
            socket,
            protocol,
            inputs,
            entry,
            chunk: std::io::Cursor::new(Vec::new()),
            complete: false,
            failure_stage: None,
        }
    }

    fn failure_stage(&self) -> Option<InputNarFailureStage> {
        self.failure_stage
    }

    fn fail(&mut self, stage: InputNarFailureStage, error: io::Error) -> io::Error {
        self.failure_stage = Some(stage);
        error
    }

    fn receive_chunk(&mut self) -> io::Result<()> {
        let body = loop {
            match self.socket.read().map_err(|_| {
                self.failure_stage = Some(InputNarFailureStage::Receive);
                io::Error::other("worker input NAR receive failed")
            })? {
                tungstenite::Message::Binary(body) => break body,
                tungstenite::Message::Ping(payload) => self
                    .socket
                    .send(tungstenite::Message::Pong(payload))
                    .map_err(|_| {
                        self.failure_stage = Some(InputNarFailureStage::KeepaliveResponse);
                        io::Error::other("worker input NAR keepalive response failed")
                    })?,
                tungstenite::Message::Close(_) => {
                    return Err(self.fail(
                        InputNarFailureStage::Message,
                        io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "worker callback closed during input NAR transfer",
                        ),
                    ));
                }
                tungstenite::Message::Text(_)
                | tungstenite::Message::Pong(_)
                | tungstenite::Message::Frame(_) => {
                    return Err(self.fail(
                        InputNarFailureStage::Message,
                        invalid("worker input NAR message is invalid"),
                    ));
                }
            }
        };
        let mut input = body.as_ref();
        let frame = read_frame(
            &mut input,
            ProtocolLimits::new(
                MAXIMUM_MANIFEST_METADATA_BYTES,
                MAXIMUM_MANIFEST_METADATA_BYTES,
            ),
        )
        .map_err(|error| self.fail(InputNarFailureStage::Frame, error))?;
        if !input.is_empty() || frame.kind() != FrameKind::InputNar {
            return Err(self.fail(
                InputNarFailureStage::Frame,
                invalid("worker input NAR frame is invalid"),
            ));
        }
        let metadata: NarMetadata =
            decode_metadata(frame.metadata(), MAXIMUM_MANIFEST_METADATA_BYTES)
                .map_err(|error| self.fail(InputNarFailureStage::Metadata, error))?;
        if metadata.path != self.entry.path {
            return Err(self.fail(
                InputNarFailureStage::Path,
                invalid("worker input NAR path is interleaved"),
            ));
        }
        self.inputs
            .receive_nar_chunk(metadata.clone(), frame.payload().len() as u64)
            .map_err(|error| self.fail(InputNarFailureStage::Transfer, error))?;
        self.protocol
            .accept(Direction::GatewayToWorker, FrameKind::InputNar)
            .map_err(|error| self.fail(InputNarFailureStage::Protocol, error))?;
        self.complete = metadata.final_chunk;
        self.chunk = std::io::Cursor::new(frame.payload().to_vec());
        Ok(())
    }

    fn finish(&self) -> io::Result<()> {
        if !self.complete || self.chunk.position() != self.chunk.get_ref().len() as u64 {
            return Err(invalid("worker input NAR is incomplete"));
        }
        Ok(())
    }
}

fn requested_inputs(manifest: &InputManifest, unresolved: PathSet) -> io::Result<PathSet> {
    Ok(PathSet {
        paths: order_requested_inputs(manifest, &unresolved)?,
    })
}

fn order_requested_inputs(
    manifest: &InputManifest,
    requested: &PathSet,
) -> io::Result<Vec<String>> {
    let requested_paths = requested
        .paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if requested_paths.len() != requested.paths.len() {
        return Err(invalid("worker input request contains duplicate paths"));
    }
    let entries = manifest
        .paths
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();
    if requested_paths
        .iter()
        .any(|path| !entries.contains_key(path.as_str()))
    {
        return Err(invalid("worker requested input is not admitted"));
    }

    let mut dependency_counts = std::collections::BTreeMap::new();
    let mut dependents = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for path in &requested_paths {
        let entry = entries
            .get(path.as_str())
            .ok_or_else(|| invalid("worker requested input is not admitted"))?;
        let dependencies = entry
            .references
            .iter()
            .filter(|reference| requested_paths.contains(*reference))
            .collect::<std::collections::BTreeSet<_>>();
        dependency_counts.insert(path.as_str(), dependencies.len());
        for dependency in dependencies {
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(path.as_str());
        }
    }

    let mut ready = dependency_counts
        .iter()
        .filter_map(|(path, count)| (*count == 0).then_some(*path))
        .collect::<std::collections::BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(requested_paths.len());
    while let Some(path) = ready.pop_first() {
        ordered.push(path.to_owned());
        for dependent in dependents.get(path).into_iter().flatten() {
            let count = dependency_counts
                .get_mut(dependent)
                .ok_or_else(|| invalid("worker input dependency is invalid"))?;
            *count = count
                .checked_sub(1)
                .ok_or_else(|| invalid("worker input dependency is invalid"))?;
            if *count == 0 {
                ready.insert(dependent);
            }
        }
    }
    if ordered.len() != requested_paths.len() {
        return Err(invalid("worker input references contain a cycle"));
    }
    Ok(ordered)
}

impl io::Read for InputNarReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.chunk.position() == self.chunk.get_ref().len() as u64 {
            if self.complete {
                return Ok(0);
            }
            self.receive_chunk()?;
        }
        std::io::Read::read(&mut self.chunk, output)
    }
}

fn startup_error(stage: &'static str, _error: io::Error) -> io::Error {
    io::Error::other(format!("worker startup failed at {stage}"))
}

pub fn authenticate(config: &WorkerConfig) -> io::Result<()> {
    connect(config).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, references: &[&str]) -> PathManifestEntry {
        PathManifestEntry {
            path: path.to_owned(),
            nar_hash: "0".repeat(64),
            nar_size: 1,
            references: references.iter().map(|value| (*value).to_owned()).collect(),
            deriver: None,
            content_address: None,
        }
    }

    fn input_reader_manifest(path: &str, nar_size: u64) -> InputManifest {
        InputManifest {
            derivation_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned(),
            build: telchar::nomad::protocol::BuildSpecification {
                derivation_path: b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_vec(),
                outputs: vec![telchar::nomad::protocol::NamedOutput {
                    name: b"out".to_vec(),
                    path: b"/nix/store/cccccccccccccccccccccccccccccccc-output".to_vec(),
                    hash_algorithm: vec![],
                    hash: vec![],
                }],
                input_sources: vec![path.as_bytes().to_vec()],
                system: "x86_64-linux".to_owned(),
                required_system_features: vec![],
                builder: b"/bin/sh".to_vec(),
                arguments: vec![],
                environment: vec![
                    (b"system".to_vec(), b"x86_64-linux".to_vec()),
                    (b"builder".to_vec(), b"/bin/sh".to_vec()),
                    (
                        b"out".to_vec(),
                        b"/nix/store/cccccccccccccccccccccccccccccccc-output".to_vec(),
                    ),
                ],
            },
            paths: vec![PathManifestEntry {
                nar_size,
                ..entry(path, &[])
            }],
            outputs: vec!["/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned()],
        }
    }

    fn requested_input_session(manifest: &InputManifest) -> InputTransferSession {
        let mut inputs = InputTransferSession::new(
            manifest.clone(),
            MAXIMUM_MANIFEST_PATHS,
            MAXIMUM_INPUT_NAR_BYTES,
            MAXIMUM_INPUT_NAR_BYTES,
        )
        .expect("input session creates");
        inputs
            .record_valid_paths(PathSet { paths: vec![] })
            .expect("valid paths record");
        inputs.request_unresolved().expect("inputs request");
        inputs
    }

    fn input_reader_protocol() -> ProtocolSession {
        let mut protocol = ProtocolSession::new();
        protocol
            .accept(Direction::WorkerToGateway, FrameKind::Authenticate)
            .expect("authentication records");
        protocol
            .accept(Direction::GatewayToWorker, FrameKind::InputManifest)
            .expect("manifest records");
        protocol
            .accept(Direction::WorkerToGateway, FrameKind::ValidPaths)
            .expect("valid paths record");
        protocol
            .accept(Direction::WorkerToGateway, FrameKind::InputRequest)
            .expect("request records");
        protocol
    }

    #[test]
    fn startup_errors_expose_only_bounded_stage() {
        let error = startup_error(
            "validation",
            io::Error::other("sensitive manifest path and payload"),
        );

        assert_eq!(error.to_string(), "worker startup failed at validation");
        assert!(!error.to_string().contains("sensitive"));
    }

    #[test]
    fn input_reader_answers_keepalive_before_nar_chunk() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let endpoint = format!("ws://{}", listener.local_addr().expect("listener address"));
        let path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input";
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("connection accepts");
            let mut socket = tungstenite::accept(stream).expect("WebSocket accepts");
            socket
                .send(tungstenite::Message::Ping(b"keepalive".to_vec().into()))
                .expect("keepalive sends");
            let metadata = NarMetadata {
                path: path.to_owned(),
                nar_hash: "0".repeat(64),
                nar_size: 3,
                offset: 0,
                final_chunk: true,
            };
            let mut message = Vec::new();
            write_frame(
                &mut message,
                &Frame::new(
                    FrameKind::InputNar,
                    encode_metadata(&metadata, MAXIMUM_MANIFEST_METADATA_BYTES)
                        .expect("metadata encodes"),
                    b"nar".to_vec(),
                ),
                ProtocolLimits::new(
                    MAXIMUM_MANIFEST_METADATA_BYTES,
                    MAXIMUM_MANIFEST_METADATA_BYTES,
                ),
            )
            .expect("frame writes");
            socket
                .send(tungstenite::Message::Binary(message.into()))
                .expect("NAR sends");
            assert!(matches!(
                socket.read().expect("keepalive response reads"),
                tungstenite::Message::Pong(_)
            ));
        });
        let (mut socket, _) = tungstenite::connect(endpoint).expect("client connects");
        let manifest = input_reader_manifest(path, 3);
        let mut inputs = requested_input_session(&manifest);
        let mut protocol = input_reader_protocol();
        let mut reader =
            InputNarReader::new(&mut socket, &mut protocol, &mut inputs, &manifest.paths[0]);
        let mut body = Vec::new();

        reader.read_to_end(&mut body).expect("NAR reads");

        assert_eq!(body, b"nar");
        assert_eq!(reader.failure_stage(), None);
        reader.finish().expect("NAR finishes");
        server.join().expect("server joins");
    }

    #[test]
    fn input_reader_records_bounded_failure_stage() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let endpoint = format!("ws://{}", listener.local_addr().expect("listener address"));
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("connection accepts");
            let mut socket = tungstenite::accept(stream).expect("WebSocket accepts");
            socket
                .send(tungstenite::Message::Pong(b"unexpected".to_vec().into()))
                .expect("unexpected control frame sends");
        });
        let (mut socket, _) = tungstenite::connect(endpoint).expect("client connects");
        let path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input";
        let manifest = input_reader_manifest(path, 1);
        let mut inputs = requested_input_session(&manifest);
        let mut protocol = input_reader_protocol();
        let mut reader =
            InputNarReader::new(&mut socket, &mut protocol, &mut inputs, &manifest.paths[0]);
        let mut byte = [0_u8; 1];

        assert!(reader.read(&mut byte).is_err());
        assert_eq!(reader.failure_stage(), Some(InputNarFailureStage::Message));
        server.join().expect("server joins");
    }

    #[test]
    fn unresolved_request_uses_dependency_order() {
        let manifest = InputManifest {
            derivation_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned(),
            build: input_reader_manifest("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-referrer", 1)
                .build,
            paths: vec![
                entry(
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-referrer",
                    &["/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference"],
                ),
                entry("/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference", &[]),
            ],
            outputs: vec!["/nix/store/cccccccccccccccccccccccccccccccc-output".to_owned()],
        };
        let unresolved = PathSet {
            paths: manifest
                .paths
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };

        assert_eq!(
            requested_inputs(&manifest, unresolved).expect("requested inputs order"),
            PathSet {
                paths: vec![
                    "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference".to_owned(),
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-referrer".to_owned(),
                ]
            }
        );
    }

    #[test]
    fn orders_requested_inputs_after_their_references() {
        let manifest = InputManifest {
            derivation_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_owned(),
            build: telchar::nomad::protocol::BuildSpecification {
                derivation_path: b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-build.drv".to_vec(),
                outputs: vec![],
                input_sources: vec![],
                system: "x86_64-linux".to_owned(),
                required_system_features: vec![],
                builder: b"/bin/sh".to_vec(),
                arguments: vec![],
                environment: vec![],
            },
            paths: vec![
                entry(
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-referrer",
                    &["/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference"],
                ),
                entry("/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference", &[]),
            ],
            outputs: vec![],
        };
        let requested = PathSet {
            paths: manifest
                .paths
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };

        assert_eq!(
            order_requested_inputs(&manifest, &requested).expect("requested inputs order"),
            vec![
                "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-reference".to_owned(),
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-referrer".to_owned(),
            ]
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityClaims {
    version: u16,
    key_id: String,
    backend: String,
    namespace: String,
    job_id: String,
    shared_build_digest: String,
    issued_at: u64,
    expires_at: u64,
    nonce: String,
    request_key: String,
}

fn decode_capability(capability: &str) -> io::Result<CapabilityClaims> {
    let (encoded, signature) = capability
        .split_once('.')
        .ok_or_else(|| invalid("worker transfer capability is invalid"))?;
    if encoded.is_empty() || signature.is_empty() || signature.contains('.') {
        return Err(invalid("worker transfer capability is invalid"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    if bytes.len() > MAXIMUM_ENVIRONMENT_VALUE_BYTES {
        return Err(invalid("worker transfer capability is invalid"));
    }
    let claims: CapabilityClaims = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("worker clock is invalid"))?
        .as_secs();
    if claims.version != 1
        || claims.key_id.is_empty()
        || claims.issued_at > claims.expires_at
        || now > claims.expires_at
    {
        return Err(invalid("worker transfer capability is invalid"));
    }
    Ok(claims)
}

#[allow(clippy::too_many_arguments)]
fn authentication_body_digest(
    backend: &str,
    namespace: &str,
    job_id: &str,
    allocation_id: &str,
    task: &str,
    shared_build_digest: &str,
    capability: &str,
    expiry: u64,
    nonce: &str,
) -> io::Result<String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "backend": backend,
        "namespace": namespace,
        "job_id": job_id,
        "allocation_id": allocation_id,
        "task": task,
        "shared_build_digest": shared_build_digest,
        "capability": capability,
        "expiry": expiry,
        "nonce": nonce,
    }))
    .map_err(|_| invalid("worker authentication body is invalid"))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(encoded)))
}

fn request_signature(
    encoded_key: &str,
    capability: &str,
    path: &str,
    body_digest: &str,
    expiry: u64,
    nonce: &str,
) -> io::Result<String> {
    let key = URL_SAFE_NO_PAD
        .decode(encoded_key)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    let mut signer = Hmac::<Sha256>::new_from_slice(&key)
        .map_err(|_| invalid("worker transfer capability is invalid"))?;
    signer.update(capability.as_bytes());
    signer.update(b"\nPOST\n");
    signer.update(path.as_bytes());
    signer.update(b"\n");
    signer.update(body_digest.as_bytes());
    signer.update(b"\n");
    signer.update(expiry.to_string().as_bytes());
    signer.update(b"\n");
    signer.update(nonce.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(signer.finalize().into_bytes()))
}

fn required(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> io::Result<String> {
    let value = lookup(name).ok_or_else(|| invalid("worker environment is incomplete"))?;
    if value.is_empty()
        || value.len() > MAXIMUM_ENVIRONMENT_VALUE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(invalid("worker environment value is invalid"));
    }
    Ok(value)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
