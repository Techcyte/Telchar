use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::config::{NomadBackendConfig, NomadCallbackConfig, NomadTransferAuthentication};
use crate::nomad_callback::{
    decode_authentication, CallbackAdmission, CallbackResolver, PostgresCallbackExecutionResolver,
    PostgresReplayAuthority,
};
use crate::nomad_callback_http::{accept_connection, CallbackHttpLimits};
use crate::nomad_transfer_authentication::{
    HmacCallbackVerifier, HmacVerificationPolicy, WorkloadIdentityPolicy, WorkloadIdentityVerifier,
};
use crate::nomad_transfer_protocol::{
    decode_metadata, encode_metadata, read_frame, write_frame, BuildOutcome, BuildResultMetadata,
    BuildSpecification, Direction, Frame, FrameKind, InputManifest, NarMetadata, OutputReceipt,
    PathManifestEntry, PathSet, ProtocolLimits, TransferSession,
};
use crate::store_closure::{backend_from_environment, StoreClosureBackend};
use crate::store_daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

pub struct NomadCallbackService {
    shutdown: Arc<AtomicBool>,
    connections: Arc<Mutex<std::collections::BTreeMap<u64, TcpStream>>>,
    workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    listener: Option<std::thread::JoinHandle<io::Result<()>>>,
    drain_timeout: std::time::Duration,
}

impl NomadCallbackService {
    pub fn start(
        listener: TcpListener,
        callback: NomadCallbackConfig,
        database_url: String,
        backends: Vec<NomadBackendConfig>,
        output_retention: std::time::Duration,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_connections = Arc::clone(&connections);
        let listener_workers = Arc::clone(&workers);
        let active = Arc::new(Mutex::new(0_usize));
        let next_connection = Arc::new(AtomicU64::new(0));
        let drain_timeout = callback.shutdown_drain_timeout();
        let listener = std::thread::spawn(move || {
            while !listener_shutdown.load(Ordering::Acquire) {
                let mut connection = match listener.accept() {
                    Ok((connection, _)) => connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let Some(permit) =
                    ConnectionPermit::acquire(Arc::clone(&active), callback.maximum_connections())
                else {
                    tracing::warn!(
                        event = "nomad.callback.connection_rejected",
                        reason = "capacity"
                    );
                    continue;
                };
                let identity = next_connection.fetch_add(1, Ordering::Relaxed);
                let shutdown_connection = connection.try_clone()?;
                let mut registered = listener_connections
                    .lock()
                    .map_err(|_| io::Error::other("Nomad callback registry lock failed"))?;
                registered.insert(identity, shutdown_connection);
                drop(registered);
                let callback = callback.clone();
                let database_url = database_url.clone();
                let backends = backends.clone();
                let connections = Arc::clone(&listener_connections);
                let worker = std::thread::spawn(move || {
                    let _permit = permit;
                    if let Err(error) = serve_connection(
                        &mut connection,
                        &callback,
                        &database_url,
                        &backends,
                        output_retention,
                    ) {
                        tracing::warn!(
                            event = "nomad.callback.connection_failed",
                            reason = error.to_string(),
                            "Nomad callback connection failed"
                        );
                    }
                    if let Ok(mut connections) = connections.lock() {
                        connections.remove(&identity);
                    }
                });
                listener_workers
                    .lock()
                    .map_err(|_| io::Error::other("Nomad callback worker lock failed"))?
                    .push(worker);
            }
            Ok(())
        });
        Ok(Self {
            shutdown,
            connections,
            workers,
            listener: Some(listener),
            drain_timeout,
        })
    }

    pub fn active_connections(&self) -> io::Result<usize> {
        Ok(self
            .connections
            .lock()
            .map_err(|_| io::Error::other("Nomad callback registry lock failed"))?
            .len())
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.shutdown.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            listener
                .join()
                .map_err(|_| io::Error::other("Nomad callback listener panicked"))??;
        }
        let deadline = Instant::now()
            .checked_add(self.drain_timeout)
            .ok_or_else(|| io::Error::other("Nomad callback drain timeout is invalid"))?;
        loop {
            let empty = self
                .connections
                .lock()
                .map_err(|_| io::Error::other("Nomad callback registry lock failed"))?
                .is_empty();
            if empty || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for connection in self
            .connections
            .lock()
            .map_err(|_| io::Error::other("Nomad callback registry lock failed"))?
            .values()
        {
            let _ = connection.shutdown(Shutdown::Both);
        }
        let workers = std::mem::take(
            &mut *self
                .workers
                .lock()
                .map_err(|_| io::Error::other("Nomad callback worker lock failed"))?,
        );
        for worker in workers {
            worker
                .join()
                .map_err(|_| io::Error::other("Nomad callback worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for NomadCallbackService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub fn serve_connection(
    connection: &mut TcpStream,
    callback: &NomadCallbackConfig,
    database_url: &str,
    backends: &[NomadBackendConfig],
    output_retention: std::time::Duration,
) -> io::Result<()> {
    let namespaces = backends
        .iter()
        .map(|backend| {
            (
                backend.target().name().to_owned(),
                backend.namespace().to_owned(),
            )
        })
        .collect();
    let resolver = CallbackResolver::new(PostgresCallbackExecutionResolver::new(
        database_url.to_owned(),
        namespaces,
    )?);
    connection.set_read_timeout(Some(callback.authentication_request_timeout()))?;
    connection.set_write_timeout(Some(callback.authentication_request_timeout()))?;
    let limits = CallbackHttpLimits::new(
        callback.maximum_header_bytes(),
        callback.maximum_body_bytes(),
    );
    let mut socket = accept_connection(connection, limits)?;
    let body = socket.read_binary()?;
    let authentication = decode_authentication(&body, callback.maximum_body_bytes())?;
    let execution = resolver
        .resolve(&authentication)?
        .ok_or_else(|| io::Error::other("Nomad callback execution is unavailable"))?;
    let backend = backends
        .iter()
        .find(|backend| backend.target().name() == authentication.backend)
        .ok_or_else(|| io::Error::other("Nomad callback backend is unavailable"))?;
    let allocation = crate::nomad_backend::NomadClient::new(backend.clone())?;
    match backend.transfer_authentication() {
        NomadTransferAuthentication::WorkloadIdentity {
            issuer,
            jwks_url,
            audience,
            ca_certificate_file,
        } => {
            let verifier = WorkloadIdentityVerifier::new(WorkloadIdentityPolicy {
                issuer: issuer.clone(),
                jwks_url: jwks_url.clone(),
                audience: audience.clone(),
                namespace: backend.namespace().to_owned(),
                job_id: authentication.job_id.clone(),
                task: "build".to_owned(),
                ca_certificate_file: ca_certificate_file.clone(),
                request_timeout: callback.authentication_request_timeout(),
                maximum_jwks_bytes: callback.maximum_jwks_bytes(),
                clock_skew: backend.transfer_limits().clock_skew(),
            })?;
            CallbackAdmission::new(
                verifier,
                allocation,
                backend.transfer_limits().maximum_frame_metadata_bytes(),
            )
            .admit(
                &body,
                "POST",
                callback_path(callback.public_url())?,
                SystemTime::now(),
            )?;
        }
        NomadTransferAuthentication::Hmac {
            key_id,
            secret_file,
        } => {
            let secret = std::fs::read(secret_file)
                .map_err(|_| io::Error::other("Nomad callback secret could not be read"))?;
            let verifier = HmacCallbackVerifier::new(HmacVerificationPolicy {
                key_id: key_id.clone(),
                secret,
                backend: authentication.backend.clone(),
                namespace: backend.namespace().to_owned(),
                job_id: authentication.job_id.clone(),
                shared_build_digest: authentication.shared_build_digest.clone(),
                task: "build".to_owned(),
                clock_skew: backend.transfer_limits().clock_skew(),
                nonce_retention: backend.transfer_limits().nonce_retention(),
                maximum_retained_nonces: callback.maximum_retained_nonces(),
            })?;
            let replay = PostgresReplayAuthority::new(
                database_url.to_owned(),
                callback.maximum_retained_nonces(),
            )?;
            CallbackAdmission::with_replay(
                verifier,
                allocation,
                replay,
                backend.transfer_limits().maximum_frame_metadata_bytes(),
            )
            .admit(
                &body,
                "POST",
                callback_path(callback.public_url())?,
                SystemTime::now(),
            )?;
        }
    }
    if !execution.matches(&authentication) {
        return Err(io::Error::other(
            "Nomad callback execution identity is inconsistent",
        ));
    }
    let derivation_path = execution
        .derivation_path()
        .ok_or_else(|| io::Error::other("Nomad callback derivation identity is unavailable"))?;
    let build_request = execution
        .build_request()
        .ok_or_else(|| io::Error::other("Nomad callback build specification is unavailable"))?;
    let limits = backend.transfer_limits();
    let connection_deadline = Instant::now()
        .checked_add(limits.maximum_connection_lifetime())
        .ok_or_else(|| io::Error::other("Nomad connection lifetime is invalid"))?;
    let keepalive_interval = limits.transfer_idle_timeout() / 2;
    socket.configure_keepalive(keepalive_interval, connection_deadline);
    socket
        .inner_mut()
        .set_read_timeout(Some(keepalive_interval))?;
    socket
        .inner_mut()
        .set_write_timeout(Some(limits.transfer_idle_timeout()))?;
    let mut closure = backend_from_environment()?;
    let manifest = input_manifest(build_request, closure.as_mut())?;
    let mut session = TransferSession::new(
        manifest.clone(),
        limits.maximum_manifest_paths(),
        limits.maximum_input_nar_bytes(),
        limits.maximum_total_input_bytes(),
        limits.maximum_output_nar_bytes(),
        limits.maximum_total_output_bytes(),
        limits.maximum_live_log_chunk_bytes(),
        limits.maximum_frame_metadata_bytes(),
    )?;
    let frame = Frame::new(
        FrameKind::InputManifest,
        encode_metadata(&manifest, limits.maximum_frame_metadata_bytes())?,
        Vec::new(),
    );
    let mut message = Vec::new();
    write_frame(
        &mut message,
        &frame,
        ProtocolLimits::new(limits.maximum_frame_metadata_bytes(), 0),
    )?;
    socket.write_binary(message)?;
    ensure_before(connection_deadline)?;
    let valid_paths = read_transfer_frame(
        &mut socket,
        ProtocolLimits::new(limits.maximum_frame_metadata_bytes(), 0),
    )?;
    session.accept(Direction::WorkerToGateway, valid_paths)?;
    ensure_before(connection_deadline)?;
    let request_frame = read_transfer_frame(
        &mut socket,
        ProtocolLimits::new(limits.maximum_frame_metadata_bytes(), 0),
    )?;
    let requested: PathSet = decode_metadata(
        request_frame.metadata(),
        limits.maximum_frame_metadata_bytes(),
    )?;
    session.accept(Direction::WorkerToGateway, request_frame)?;
    stream_requested_inputs(
        &mut socket,
        &mut session,
        &manifest,
        &requested,
        limits,
        connection_deadline,
    )?;
    let outcome = match receive_build_outputs(
        &mut socket,
        &mut session,
        database_url,
        derivation_path,
        build_request,
        limits,
        connection_deadline,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = crate::persistence::complete_shared_build_failure(
                database_url,
                derivation_path,
                "nomad-transfer-failure",
                &serde_json::json!({"stage": "output-collection"}),
                output_retention,
            );
            return Err(error);
        }
    };
    if let BuildCollectionOutcome::Failed { diagnostic } = outcome {
        crate::persistence::complete_shared_build_failure(
            database_url,
            derivation_path,
            "nomad-build-failure",
            &serde_json::json!({"diagnostic": diagnostic}),
            output_retention,
        )
        .map_err(|_| io::Error::other("Nomad shared build failure completion failed"))?;
        return Ok(());
    }
    crate::persistence::complete_shared_build_success(
        database_url,
        derivation_path,
        &serde_json::json!({
            "status": "built",
            "outputs": build_request.expected_outputs().iter().map(|(name, path)| {
                serde_json::json!({
                    "name": String::from_utf8_lossy(name),
                    "path": String::from_utf8_lossy(path),
                })
            }).collect::<Vec<_>>(),
            "backend": authentication.backend,
            "execution_id": authentication.job_id,
        }),
        output_retention,
    )
    .map_err(|_| io::Error::other("Nomad shared build completion failed"))?;
    Ok(())
}

fn read_transfer_frame<S: io::Read + io::Write>(
    socket: &mut crate::nomad_callback_http::CallbackSocket<S>,
    limits: ProtocolLimits,
) -> io::Result<Frame> {
    let body = socket.read_binary()?;
    let mut input = body.as_slice();
    let frame = read_frame(&mut input, limits)?;
    if !input.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Nomad transfer message contains trailing bytes",
        ));
    }
    Ok(frame)
}

enum BuildCollectionOutcome {
    Built,
    Failed { diagnostic: Option<String> },
}

fn receive_build_outputs<S: io::Read + io::Write>(
    socket: &mut crate::nomad_callback_http::CallbackSocket<S>,
    session: &mut TransferSession,
    database_url: &str,
    derivation_path: &str,
    build_request: &crate::build_request::BuildRequest,
    limits: crate::config::NomadTransferLimits,
    connection_deadline: Instant,
) -> io::Result<BuildCollectionOutcome> {
    let endpoint = gateway_store_endpoint()?;
    let mut current: Option<OutputImport> = None;
    let mut collecting = false;
    loop {
        ensure_before(connection_deadline)?;
        let frame = read_transfer_frame(
            socket,
            ProtocolLimits::new(
                limits.maximum_frame_metadata_bytes(),
                limits.stream_buffer_bytes(),
            ),
        )?;
        match frame.kind() {
            FrameKind::BuildStarted | FrameKind::LogChunk => {
                session.accept(Direction::WorkerToGateway, frame)?;
            }
            FrameKind::OutputMetadata => {
                if current.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Nomad output NAR transfer is interleaved",
                    ));
                }
                if !collecting {
                    crate::persistence::collect_shared_build(database_url, derivation_path)
                        .map_err(|_| {
                            io::Error::other("Nomad output collection transition failed")
                        })?;
                    collecting = true;
                }
                let metadata: PathManifestEntry =
                    decode_metadata(frame.metadata(), limits.maximum_frame_metadata_bytes())?;
                session.accept(Direction::WorkerToGateway, frame)?;
                current = Some(OutputImport::new(
                    &endpoint,
                    metadata,
                    limits.stream_buffer_bytes(),
                )?);
            }
            FrameKind::OutputNar => {
                let metadata: NarMetadata =
                    decode_metadata(frame.metadata(), limits.maximum_frame_metadata_bytes())?;
                let import = current.as_mut().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Nomad output NAR has no metadata",
                    )
                })?;
                import.receive(&metadata, frame.payload())?;
                session.accept(Direction::WorkerToGateway, frame)?;
                if metadata.final_chunk {
                    let receipt = import.finish()?;
                    let receipt_frame = Frame::new(
                        FrameKind::OutputReceipt,
                        encode_metadata(&receipt, limits.maximum_frame_metadata_bytes())?,
                        Vec::new(),
                    );
                    session.accept(Direction::GatewayToWorker, receipt_frame.clone())?;
                    write_transfer_frame(socket, &receipt_frame, limits, 0)?;
                    current = None;
                }
            }
            FrameKind::BuildResult => {
                if current.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Nomad build completed during output transfer",
                    ));
                }
                let result: BuildResultMetadata =
                    decode_metadata(frame.metadata(), limits.maximum_frame_metadata_bytes())?;
                session.accept(Direction::WorkerToGateway, frame)?;
                if result.outcome == BuildOutcome::Failed {
                    return Ok(BuildCollectionOutcome::Failed {
                        diagnostic: result.diagnostic,
                    });
                }
                for (_, path) in build_request.expected_outputs() {
                    let mut store = GatewayStoreConnection::connect(&endpoint)?;
                    if !store.is_valid_path(path)? {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Nomad output is unavailable in gateway store",
                        ));
                    }
                }
                return Ok(BuildCollectionOutcome::Built);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Nomad transfer frame is invalid during build output collection",
                ));
            }
        }
    }
}

fn write_transfer_frame<S: io::Read + io::Write>(
    socket: &mut crate::nomad_callback_http::CallbackSocket<S>,
    frame: &Frame,
    limits: crate::config::NomadTransferLimits,
    maximum_payload_bytes: usize,
) -> io::Result<()> {
    let mut message = Vec::new();
    write_frame(
        &mut message,
        frame,
        ProtocolLimits::new(limits.maximum_frame_metadata_bytes(), maximum_payload_bytes),
    )?;
    socket.write_binary(message)
}

fn stream_requested_inputs<S: io::Read + io::Write>(
    socket: &mut crate::nomad_callback_http::CallbackSocket<S>,
    session: &mut TransferSession,
    manifest: &InputManifest,
    requested: &PathSet,
    limits: crate::config::NomadTransferLimits,
    connection_deadline: Instant,
) -> io::Result<()> {
    let endpoint = gateway_store_endpoint()?;
    let mut store = GatewayStoreConnection::connect(&endpoint)?;
    for requested_path in &requested.paths {
        ensure_before(connection_deadline)?;
        let entry = manifest
            .paths
            .iter()
            .find(|entry| &entry.path == requested_path)
            .ok_or_else(|| io::Error::other("Nomad input request is not admitted"))?;
        let mut sink = InputNarSink {
            socket,
            session,
            entry,
            offset: 0,
            chunk: Vec::with_capacity(limits.stream_buffer_bytes()),
            maximum_metadata_bytes: limits.maximum_frame_metadata_bytes(),
        };
        store.nar_from_path(entry.path.as_bytes(), entry.nar_size, &mut sink)?;
        sink.finish()?;
    }
    Ok(())
}

struct InputNarSink<'a, S: io::Read + io::Write> {
    socket: &'a mut crate::nomad_callback_http::CallbackSocket<S>,
    session: &'a mut TransferSession,
    entry: &'a PathManifestEntry,
    offset: u64,
    chunk: Vec<u8>,
    maximum_metadata_bytes: usize,
}

impl<S: io::Read + io::Write> InputNarSink<'_, S> {
    fn send_chunk(&mut self, final_chunk: bool) -> io::Result<()> {
        if self.chunk.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad input NAR chunk is empty",
            ));
        }
        let metadata = NarMetadata {
            path: self.entry.path.clone(),
            nar_hash: self.entry.nar_hash.clone(),
            nar_size: self.entry.nar_size,
            offset: self.offset,
            final_chunk,
        };
        let frame = Frame::new(
            FrameKind::InputNar,
            encode_metadata(&metadata, self.maximum_metadata_bytes)?,
            std::mem::take(&mut self.chunk),
        );
        self.offset = self
            .offset
            .checked_add(frame.payload().len() as u64)
            .ok_or_else(|| io::Error::other("Nomad input NAR offset overflow"))?;
        self.session
            .accept(Direction::GatewayToWorker, frame.clone())?;
        let mut message = Vec::new();
        write_frame(
            &mut message,
            &frame,
            ProtocolLimits::new(self.maximum_metadata_bytes, frame.payload().len()),
        )?;
        self.socket.write_binary(message)
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.offset + self.chunk.len() as u64 != self.entry.nar_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad input NAR length is invalid",
            ));
        }
        self.send_chunk(true)
    }
}

impl<S: io::Read + io::Write> io::Write for InputNarSink<'_, S> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_length = input.len();
        while !input.is_empty() {
            let capacity = self.chunk.capacity() - self.chunk.len();
            let copied = capacity.min(input.len());
            self.chunk.extend_from_slice(&input[..copied]);
            input = &input[copied..];
            if self.chunk.len() == self.chunk.capacity()
                && self.offset + (self.chunk.len() as u64) < self.entry.nar_size
            {
                self.send_chunk(false)?;
                self.chunk = Vec::with_capacity(self.chunk.capacity());
            }
        }
        Ok(input_length)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn ensure_before(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Nomad connection lifetime exceeded",
        ));
    }
    Ok(())
}

fn gateway_store_endpoint() -> io::Result<GatewayStoreEndpoint> {
    std::env::var_os("TELCHAR_GATEWAY_STORE_URI")
        .ok_or_else(|| io::Error::other("gateway store endpoint is not configured"))
        .and_then(|value| GatewayStoreEndpoint::parse_os(&value))
}

struct OutputImport {
    metadata: PathManifestEntry,
    store: GatewayStoreConnection,
    temporary: tempfile::SpooledTempFile,
    received: u64,
}

impl OutputImport {
    fn new(
        endpoint: &GatewayStoreEndpoint,
        metadata: PathManifestEntry,
        memory_bytes: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            metadata,
            store: GatewayStoreConnection::connect(endpoint)?,
            temporary: tempfile::SpooledTempFile::new(memory_bytes),
            received: 0,
        })
    }

    fn receive(&mut self, metadata: &NarMetadata, payload: &[u8]) -> io::Result<()> {
        if metadata.path != self.metadata.path
            || metadata.nar_hash != self.metadata.nar_hash
            || metadata.nar_size != self.metadata.nar_size
            || metadata.offset != self.received
            || payload.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad output NAR chunk is inconsistent",
            ));
        }
        io::Write::write_all(&mut self.temporary, payload)?;
        self.received = self
            .received
            .checked_add(payload.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Nomad output too large"))?;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<OutputReceipt> {
        if self.received != self.metadata.nar_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad output NAR length is inconsistent",
            ));
        }
        io::Seek::rewind(&mut self.temporary)?;
        let references = self
            .metadata
            .references
            .iter()
            .map(|reference| reference.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let info = nix_worker_protocol::AddToStoreNarInfo {
            path: self.metadata.path.as_bytes(),
            deriver: self.metadata.deriver.as_deref().map(str::as_bytes),
            nar_hash_hex: &self.metadata.nar_hash,
            references: &references,
            registration_time: 0,
            nar_size: self.metadata.nar_size,
            ultimate: false,
            signatures: &[],
            content_address: None,
        };
        self.store
            .add_to_store_nar(&info, &mut self.temporary, false, true)?;
        if !self.store.is_valid_path(self.metadata.path.as_bytes())? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Nomad output import verification failed",
            ));
        }
        Ok(OutputReceipt {
            path: self.metadata.path.clone(),
            accepted: true,
        })
    }
}

fn input_manifest(
    build_request: &crate::build_request::BuildRequest,
    closure: &mut dyn StoreClosureBackend,
) -> io::Result<InputManifest> {
    build_request.validate_for_execution()?;
    let mut roots = build_request.input_sources().to_vec();
    roots.push(build_request.derivation_path().to_vec());
    let closure_paths = closure.input_closure(&roots)?;
    let closure_identities = closure_paths
        .iter()
        .map(|path| path.store_path.as_bytes())
        .collect::<std::collections::BTreeSet<_>>();
    if !roots
        .iter()
        .all(|root| closure_identities.contains(root.as_slice()))
    {
        return Err(io::Error::other(
            "Nomad callback input closure is incomplete",
        ));
    }
    let paths = closure_paths
        .into_iter()
        .map(|path| PathManifestEntry {
            path: path.store_path,
            nar_hash: path.nar_hash,
            nar_size: path.nar_size,
            references: path.references,
            deriver: path.deriver,
        })
        .collect();
    let derivation_path = std::str::from_utf8(build_request.derivation_path())
        .map_err(|_| io::Error::other("Nomad callback derivation path is invalid"))?
        .to_owned();
    let outputs = build_request
        .expected_outputs()
        .iter()
        .map(|(_, path)| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|_| io::Error::other("Nomad callback output path is invalid"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(InputManifest {
        derivation_path,
        build: BuildSpecification::from(build_request),
        paths,
        outputs,
    })
}

fn callback_path(public_url: &str) -> io::Result<&str> {
    let scheme = public_url
        .find("://")
        .ok_or_else(|| io::Error::other("Nomad callback URL is invalid"))?;
    let authority = &public_url[scheme + 3..];
    Ok(authority
        .find('/')
        .map_or("/", |offset| &authority[offset..]))
}

struct ConnectionPermit {
    active: Arc<Mutex<usize>>,
}

impl ConnectionPermit {
    fn acquire(active: Arc<Mutex<usize>>, maximum: usize) -> Option<Self> {
        let mut count = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *count >= maximum {
            return None;
        }
        *count += 1;
        drop(count);
        Some(Self { active })
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut count = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *count = count.saturating_sub(1);
    }
}
