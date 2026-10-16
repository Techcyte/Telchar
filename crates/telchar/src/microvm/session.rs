//! Bounded transfer for a caller-admitted MicroVM execution.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use tungstenite::{Message, WebSocket};

use super::outputs::Outputs;
use crate::nomad::protocol::{
    BuildResultMetadata, Direction, Frame, FrameKind, InputManifest, NarMetadata,
    PathManifestEntry, PathSet, ProtocolLimits, TransferSession, decode_metadata, encode_metadata,
    read_frame, write_frame,
};
use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[derive(Clone, Copy)]
pub struct Limits {
    pub maximum_paths: usize,
    pub maximum_nar_bytes: u64,
    pub maximum_total_bytes: u64,
    pub metadata_bytes: usize,
    pub chunk_bytes: usize,
}

pub fn run_admitted(
    connection: super::rendezvous::AdmittedConnection,
    owner: &crate::service::singleton_ownership::SingletonOwnership,
    store: GatewayStoreEndpoint,
    staging: PathBuf,
    limits: Limits,
    log: impl FnMut(&[u8]),
) -> io::Result<BuildResultMetadata> {
    let (intent_id, socket) = connection.into_parts();
    let load = || {
        owner.verify().map_err(io::Error::other)?;
        crate::persistence::microvm_session_request(owner.database(), &intent_id)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("MicroVM execution is not active"))
    };
    let request = load()?;
    let session = Session::new(&request, store, staging, limits)?;
    let interrupt = socket.get_ref().interrupt_handle()?;
    std::thread::scope(|scope| {
        let (stop, stopped) = std::sync::mpsc::channel();
        let active_request = &request;
        let load_request = &load;
        let monitor = std::thread::Builder::new()
            .name("microvm-authority".into())
            .spawn_scoped(scope, move || {
                loop {
                    match stopped.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if load_request().map_or(true, |active| &active != active_request) {
                        let _ = interrupt.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                }
            })?;
        let result = session.run(
            socket,
            || {
                if load()? != request {
                    return Err(io::Error::other("MicroVM execution authority changed"));
                }
                Ok(())
            },
            log,
        );
        drop(stop);
        monitor
            .join()
            .map_err(|_| io::Error::other("MicroVM authority monitor failed"))?;
        result
    })
}

pub struct Session {
    manifest: InputManifest,
    request: crate::build::BuildRequest,
    store: GatewayStoreEndpoint,
    transfer: TransferSession,
    outputs: Outputs,
    limits: Limits,
}

impl Session {
    pub fn new(
        request: &crate::build::BuildRequest,
        store: GatewayStoreEndpoint,
        staging: PathBuf,
        limits: Limits,
    ) -> io::Result<Self> {
        if limits.chunk_bytes == 0
            || limits.chunk_bytes > 256 * 1024
            || limits.metadata_bytes == 0
            || limits.metadata_bytes > 8 * 1024 * 1024
        {
            return Err(io::Error::other("MicroVM transfer limits are invalid"));
        }
        use crate::store::closure::StoreClosureBackend;
        request.validate_for_execution()?;
        let paths = crate::store::closure::GatewayStoreClosureBackend::new(store.clone())
            .input_closure(request.input_sources())?
            .into_iter()
            .map(|path| PathManifestEntry {
                path: path.store_path,
                nar_hash: path.nar_hash,
                nar_size: path.nar_size,
                references: path.references,
                deriver: path.deriver,
                content_address: path.content_address,
            })
            .collect();
        let text = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| io::Error::other("MicroVM admitted path is invalid"))
        };
        let manifest = InputManifest {
            derivation_path: text(request.derivation_path())?,
            build: crate::nomad::protocol::BuildSpecification::from(request),
            paths,
            outputs: request
                .expected_outputs()
                .iter()
                .map(|(_, path)| text(path))
                .collect::<io::Result<_>>()?,
        };
        let transfer = TransferSession::new(
            manifest.clone(),
            limits.maximum_paths,
            limits.maximum_nar_bytes,
            limits.maximum_total_bytes,
            limits.maximum_nar_bytes,
            limits.maximum_total_bytes,
            limits.chunk_bytes,
            limits.metadata_bytes,
        )?;
        let outputs = Outputs::new(
            manifest.outputs.clone(),
            store.clone(),
            staging,
            limits.maximum_nar_bytes,
            limits.maximum_total_bytes,
        )?;
        Ok(Self {
            manifest,
            request: request.clone(),
            store,
            transfer,
            outputs,
            limits,
        })
    }

    /// The caller supplies transport deadlines and a cancellation/ownership fence.
    pub fn run<S: Read + Write>(
        mut self,
        mut socket: WebSocket<S>,
        mut verify: impl FnMut() -> io::Result<()>,
        mut log: impl FnMut(&[u8]),
    ) -> io::Result<BuildResultMetadata> {
        verify()?;
        let maximum = self.limits.metadata_bytes + self.limits.chunk_bytes + 16;
        socket.set_config(|config| {
            config.max_message_size = Some(maximum);
            config.max_frame_size = Some(maximum);
        });
        send(
            &mut socket,
            &Frame::new(
                FrameKind::InputManifest,
                encode_metadata(&self.manifest, self.limits.metadata_bytes)?,
                vec![],
            ),
            self.limits,
        )?;
        for expected in [FrameKind::ValidPaths, FrameKind::InputRequest] {
            verify()?;
            let frame = receive(&mut socket, self.limits)?;
            verify()?;
            if frame.kind() != expected {
                return Err(io::Error::other("MicroVM input resolution is out of order"));
            }
            self.transfer
                .accept(Direction::WorkerToGateway, frame.clone())?;
            if expected == FrameKind::InputRequest {
                let requested: PathSet =
                    decode_metadata(frame.metadata(), self.limits.metadata_bytes)?;
                if !requested.paths.is_empty() {
                    let mut store = GatewayStoreConnection::connect(&self.store)?;
                    for path in requested.paths {
                        verify()?;
                        let entry = self
                            .manifest
                            .paths
                            .iter()
                            .find(|entry| entry.path == path)
                            .ok_or_else(|| io::Error::other("MicroVM input is not admitted"))?;
                        let mut sink = InputSink {
                            socket: &mut socket,
                            transfer: &mut self.transfer,
                            entry,
                            offset: 0,
                            limits: self.limits,
                            verify: &mut verify,
                        };
                        store.nar_from_path(path.as_bytes(), entry.nar_size, &mut sink)?;
                        if sink.offset != entry.nar_size {
                            return Err(io::Error::other("MicroVM input NAR is incomplete"));
                        }
                    }
                }
            }
        }
        loop {
            verify()?;
            let frame = receive(&mut socket, self.limits)?;
            verify()?;
            self.transfer
                .accept(Direction::WorkerToGateway, frame.clone())?;
            match frame.kind() {
                FrameKind::BuildStarted => {}
                FrameKind::LogChunk => log(frame.payload()),
                FrameKind::OutputMetadata => {
                    let metadata: PathManifestEntry =
                        decode_metadata(frame.metadata(), self.limits.metadata_bytes)?;
                    if metadata.references.iter().any(|reference| {
                        !self
                            .manifest
                            .paths
                            .iter()
                            .any(|input| &input.path == reference)
                            && !self.manifest.outputs.contains(reference)
                    }) {
                        return Err(io::Error::other("MicroVM output reference is not admitted"));
                    }
                    if metadata
                        .deriver
                        .as_ref()
                        .is_some_and(|deriver| deriver != &self.manifest.derivation_path)
                    {
                        return Err(io::Error::other("MicroVM output deriver is not admitted"));
                    }
                    let authority = self
                        .request
                        .output_authorities()
                        .iter()
                        .find(|output| output.path() == metadata.path.as_bytes())
                        .ok_or_else(|| io::Error::other("MicroVM output is not admitted"))?;
                    if authority.expected_content_address() != metadata.content_address {
                        return Err(io::Error::other(
                            "MicroVM output content address is not admitted",
                        ));
                    }
                    self.outputs.declare(metadata)?;
                }
                FrameKind::OutputNar => {
                    let chunk: NarMetadata =
                        decode_metadata(frame.metadata(), self.limits.metadata_bytes)?;
                    if let Some(receipt) = self.outputs.receive(&chunk, frame.payload())? {
                        verify()?;
                        let receipt = Frame::new(
                            FrameKind::OutputReceipt,
                            encode_metadata(&receipt, self.limits.metadata_bytes)?,
                            vec![],
                        );
                        self.transfer
                            .accept(Direction::GatewayToWorker, receipt.clone())?;
                        send(&mut socket, &receipt, self.limits)?;
                    }
                }
                FrameKind::BuildResult => {
                    let result = decode_metadata(frame.metadata(), self.limits.metadata_bytes)?;
                    self.outputs.finish(&result)?;
                    verify()?;
                    return Ok(result);
                }
                _ => return Err(io::Error::other("MicroVM execution frame is out of order")),
            }
        }
    }
}

fn send<S: Read + Write>(
    socket: &mut WebSocket<S>,
    frame: &Frame,
    limits: Limits,
) -> io::Result<()> {
    let mut message = Vec::new();
    write_frame(
        &mut message,
        frame,
        ProtocolLimits::new(limits.metadata_bytes, limits.chunk_bytes),
    )?;
    socket
        .send(Message::Binary(message.into()))
        .map_err(io::Error::other)
}

fn receive<S: Read + Write>(socket: &mut WebSocket<S>, limits: Limits) -> io::Result<Frame> {
    let Message::Binary(message) = socket.read().map_err(io::Error::other)? else {
        return Err(io::Error::other(
            "MicroVM transfer requires binary messages",
        ));
    };
    let mut input = message.as_ref();
    let frame = read_frame(
        &mut input,
        ProtocolLimits::new(limits.metadata_bytes, limits.chunk_bytes),
    )?;
    if !input.is_empty() {
        return Err(io::Error::other(
            "MicroVM transfer message has trailing bytes",
        ));
    }
    Ok(frame)
}

struct InputSink<'a, S: Read + Write, F: FnMut() -> io::Result<()>> {
    socket: &'a mut WebSocket<S>,
    transfer: &'a mut TransferSession,
    entry: &'a PathManifestEntry,
    offset: u64,
    limits: Limits,
    verify: &'a mut F,
}

impl<S: Read + Write, F: FnMut() -> io::Result<()>> Write for InputSink<'_, S, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(self.limits.chunk_bytes) {
            (self.verify)()?;
            let end = self
                .offset
                .checked_add(chunk.len() as u64)
                .filter(|end| *end <= self.entry.nar_size)
                .ok_or_else(|| io::Error::other("MicroVM input exceeds declaration"))?;
            let metadata = NarMetadata {
                path: self.entry.path.clone(),
                nar_hash: self.entry.nar_hash.clone(),
                nar_size: self.entry.nar_size,
                offset: self.offset,
                final_chunk: end == self.entry.nar_size,
            };
            let frame = Frame::new(
                FrameKind::InputNar,
                encode_metadata(&metadata, self.limits.metadata_bytes)?,
                chunk.to_vec(),
            );
            self.transfer
                .accept(Direction::GatewayToWorker, frame.clone())?;
            send(self.socket, &frame, self.limits)?;
            self.offset = end;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
