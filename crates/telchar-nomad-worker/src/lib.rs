use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use telchar::nomad_transfer_protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, Authentication, AuthenticationProof,
    BuildStarted, Direction, Frame, FrameKind, InputManifest, InputTransferSession, LogChunk,
    NarMetadata, PathSet, ProtocolLimits, ProtocolSession,
};
use telchar::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};
use tungstenite::client::IntoClientRequest;
use url::Url;

const MAXIMUM_ENVIRONMENT_VALUE_BYTES: usize = 4096;
const MAXIMUM_AUTHENTICATION_METADATA_BYTES: usize = 16 * 1024;
const MAXIMUM_MANIFEST_METADATA_BYTES: usize = 1024 * 1024;
const MAXIMUM_MANIFEST_PATHS: usize = nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES;
const MAXIMUM_INPUT_NAR_BYTES: u64 = 16 * 1024 * 1024 * 1024;

type WorkerSocket =
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

pub struct WorkerSession {
    socket: WorkerSocket,
    manifest: InputManifest,
    protocol: ProtocolSession,
    inputs: InputTransferSession,
}

impl WorkerSession {
    pub fn manifest(&self) -> &InputManifest {
        &self.manifest
    }

    pub fn into_parts(self) -> (WorkerSocket, InputManifest) {
        (self.socket, self.manifest)
    }

    pub fn resolve_inputs(&mut self, store_uri: &str) -> io::Result<PathSet> {
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
        let requested = PathSet {
            paths: self
                .manifest
                .paths
                .iter()
                .map(|entry| &entry.path)
                .filter(|path| !available.contains(path))
                .cloned()
                .collect(),
        };
        if requested != self.inputs.request_unresolved()? {
            return Err(invalid("worker unresolved input set is inconsistent"));
        }
        self.send_metadata(FrameKind::InputRequest, &requested)?;
        Ok(requested)
    }

    pub fn build(&mut self, store_uri: &str) -> io::Result<nix_worker_protocol::WorkerBuildResult> {
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

    pub fn import_requested_inputs(
        &mut self,
        store_uri: &str,
        requested: &PathSet,
    ) -> io::Result<()> {
        let endpoint = GatewayStoreEndpoint::parse(store_uri)
            .map_err(|_| invalid("worker Nix store URI is invalid"))?;
        let mut store = GatewayStoreConnection::connect(&endpoint)
            .map_err(|_| io::Error::other("worker Nix store connection failed"))?;
        for path in &requested.paths {
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
            let mut source = InputNarReader::new(
                &mut self.socket,
                &mut self.protocol,
                &mut self.inputs,
                entry,
            );
            store
                .add_to_store_nar(&info, &mut source, false, true)
                .map_err(|_| io::Error::other("worker input import failed"))?;
            source.finish()?;
        }
        self.inputs.ready_to_build()
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

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    endpoint: Url,
    store_uri: String,
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
                        token: required(&mut lookup, "NOMAD_TOKEN")?,
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
    let (mut socket, response) = tungstenite::connect(request)
        .map_err(|_| io::Error::other("worker callback connection failed"))?;
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

pub fn receive_manifest(config: &WorkerConfig) -> io::Result<WorkerSession> {
    let mut socket = connect(config)?;
    let message = loop {
        match socket
            .read()
            .map_err(|_| io::Error::other("worker manifest receive failed"))?
        {
            tungstenite::Message::Binary(body) => break body,
            tungstenite::Message::Ping(payload) => socket
                .send(tungstenite::Message::Pong(payload))
                .map_err(|_| io::Error::other("worker manifest receive failed"))?,
            tungstenite::Message::Close(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "worker callback closed before manifest",
                ));
            }
            tungstenite::Message::Text(_)
            | tungstenite::Message::Pong(_)
            | tungstenite::Message::Frame(_) => {
                return Err(invalid("worker manifest message is invalid"));
            }
        }
    };
    let mut input = message.as_ref();
    let frame = read_frame(
        &mut input,
        ProtocolLimits::new(MAXIMUM_MANIFEST_METADATA_BYTES, 0),
    )?;
    if !input.is_empty() || frame.kind() != FrameKind::InputManifest || !frame.payload().is_empty()
    {
        return Err(invalid("worker manifest frame is invalid"));
    }
    let manifest: InputManifest =
        decode_metadata(frame.metadata(), MAXIMUM_MANIFEST_METADATA_BYTES)?;
    manifest.validate(MAXIMUM_MANIFEST_PATHS, MAXIMUM_INPUT_NAR_BYTES)?;
    let inputs = InputTransferSession::new(
        manifest.clone(),
        MAXIMUM_MANIFEST_PATHS,
        MAXIMUM_INPUT_NAR_BYTES,
        MAXIMUM_INPUT_NAR_BYTES
            .checked_mul(MAXIMUM_MANIFEST_PATHS as u64)
            .ok_or_else(|| invalid("worker manifest limit is invalid"))?,
    )?;
    let mut protocol = ProtocolSession::new();
    protocol.accept(Direction::WorkerToGateway, FrameKind::Authenticate)?;
    protocol.accept(Direction::GatewayToWorker, FrameKind::InputManifest)?;
    Ok(WorkerSession {
        socket,
        manifest,
        protocol,
        inputs,
    })
}

struct InputNarReader<'a> {
    socket: &'a mut WorkerSocket,
    protocol: &'a mut ProtocolSession,
    inputs: &'a mut InputTransferSession,
    entry: &'a telchar::nomad_transfer_protocol::PathManifestEntry,
    chunk: std::io::Cursor<Vec<u8>>,
    complete: bool,
}

impl<'a> InputNarReader<'a> {
    fn new(
        socket: &'a mut WorkerSocket,
        protocol: &'a mut ProtocolSession,
        inputs: &'a mut InputTransferSession,
        entry: &'a telchar::nomad_transfer_protocol::PathManifestEntry,
    ) -> Self {
        Self {
            socket,
            protocol,
            inputs,
            entry,
            chunk: std::io::Cursor::new(Vec::new()),
            complete: false,
        }
    }

    fn receive_chunk(&mut self) -> io::Result<()> {
        let message = self
            .socket
            .read()
            .map_err(|_| io::Error::other("worker input NAR receive failed"))?;
        let tungstenite::Message::Binary(body) = message else {
            return Err(invalid("worker input NAR message is invalid"));
        };
        let mut input = body.as_ref();
        let frame = read_frame(
            &mut input,
            ProtocolLimits::new(
                MAXIMUM_MANIFEST_METADATA_BYTES,
                MAXIMUM_MANIFEST_METADATA_BYTES,
            ),
        )?;
        if !input.is_empty() || frame.kind() != FrameKind::InputNar {
            return Err(invalid("worker input NAR frame is invalid"));
        }
        let metadata: NarMetadata =
            decode_metadata(frame.metadata(), MAXIMUM_MANIFEST_METADATA_BYTES)?;
        if metadata.path != self.entry.path {
            return Err(invalid("worker input NAR path is interleaved"));
        }
        self.inputs
            .receive_nar_chunk(metadata.clone(), frame.payload().len() as u64)?;
        self.protocol
            .accept(Direction::GatewayToWorker, FrameKind::InputNar)?;
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

pub fn authenticate(config: &WorkerConfig) -> io::Result<()> {
    connect(config).map(|_| ())
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
