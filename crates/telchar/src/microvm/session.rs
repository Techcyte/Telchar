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

pub struct Session {
    manifest: InputManifest,
    store: GatewayStoreEndpoint,
    transfer: TransferSession,
    outputs: Outputs,
    limits: Limits,
}

impl Session {
    pub fn new(
        manifest: InputManifest,
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
