//! Stages exact execution outputs and acknowledges authoritative gateway registration.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::nomad::protocol::{
    BuildResultMetadata, NarMetadata, OutputReceipt, OutputTransferSession, PathManifestEntry,
};
use crate::store::daemon::GatewayStoreEndpoint;
use crate::store::promotion::{
    DeclaredPathInfo, GatewayStorePromotionBackend, validate_and_promote_nar,
};

pub struct Outputs {
    session: OutputTransferSession,
    endpoint: GatewayStoreEndpoint,
    staging: PathBuf,
    current: Option<(PathManifestEntry, std::fs::File)>,
}

impl Outputs {
    pub fn new(
        expected: Vec<String>,
        endpoint: GatewayStoreEndpoint,
        staging: PathBuf,
        maximum_nar_bytes: u64,
        maximum_total_bytes: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            session: OutputTransferSession::new(
                expected,
                10000,
                maximum_nar_bytes,
                maximum_total_bytes,
                4096,
            )?,
            endpoint,
            staging,
            current: None,
        })
    }

    pub fn declare(&mut self, metadata: PathManifestEntry) -> io::Result<()> {
        if self.current.is_some() {
            return Err(io::Error::other("interleaved MicroVM output"));
        }
        self.session.declare(metadata.clone())?;
        self.current = Some((metadata, tempfile::tempfile_in(&self.staging)?));
        Ok(())
    }

    pub fn receive(
        &mut self,
        chunk: &NarMetadata,
        payload: &[u8],
    ) -> io::Result<Option<OutputReceipt>> {
        let (metadata, file) = self
            .current
            .as_mut()
            .ok_or_else(|| io::Error::other("MicroVM output metadata missing"))?;
        if chunk.path != metadata.path
            || chunk.nar_hash != metadata.nar_hash
            || chunk.nar_size != metadata.nar_size
        {
            return Err(io::Error::other("MicroVM output chunk metadata mismatch"));
        }
        self.session.receive_nar_chunk(
            &chunk.path,
            chunk.offset,
            payload.len() as u64,
            chunk.final_chunk,
        )?;
        file.write_all(payload)?;
        if !chunk.final_chunk {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut hash = [0; 32];
        for (byte, pair) in hash
            .iter_mut()
            .zip(metadata.nar_hash.as_bytes().chunks_exact(2))
        {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(io::Error::other)?, 16)
                .map_err(io::Error::other)?;
        }
        let declared = DeclaredPathInfo {
            path: metadata.path.clone().into(),
            nar_hash: hash,
            nar_size: metadata.nar_size,
            references: metadata.references.iter().map(PathBuf::from).collect(),
            deriver: metadata.deriver.as_ref().map(PathBuf::from),
            content_address: metadata.content_address.clone(),
            signatures: vec![],
            ultimate: false,
        };
        let mut backend = GatewayStorePromotionBackend::new(self.endpoint.clone());
        validate_and_promote_nar(
            file,
            &self.staging,
            Path::new("/nix/store"),
            &declared,
            &mut backend,
        )?;
        let receipt = OutputReceipt {
            path: metadata.path.clone(),
            accepted: true,
        };
        self.session.record_receipt(receipt.clone())?;
        self.current = None;
        Ok(Some(receipt))
    }

    pub fn finish(&mut self, result: &BuildResultMetadata) -> io::Result<()> {
        self.session.finish(result)
    }
}
