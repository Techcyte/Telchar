//! Queries authoritative gateway-store path validity through the daemon protocol.

use std::io;

use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingPaths {
    pub will_build: Vec<Vec<u8>>,
    pub will_substitute: Vec<Vec<u8>>,
    pub unknown: Vec<Vec<u8>>,
    pub download_size: u64,
    pub nar_size: u64,
}

pub trait QueryValidPathsStore {
    fn query_valid_paths(
        &mut self,
        paths: &[Vec<u8>],
        substitute: bool,
    ) -> io::Result<Vec<Vec<u8>>>;
    fn query_missing(&mut self, targets: &[Vec<u8>]) -> io::Result<MissingPaths>;
}

pub struct GatewayStoreQuery {
    endpoint: Option<GatewayStoreEndpoint>,
}

impl GatewayStoreQuery {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self::with_endpoint(Some(endpoint))
    }

    pub fn with_endpoint(endpoint: Option<GatewayStoreEndpoint>) -> Self {
        Self { endpoint }
    }

    fn endpoint(&self) -> io::Result<&GatewayStoreEndpoint> {
        self.endpoint.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "gateway store endpoint is not configured",
            )
        })
    }
}

impl QueryValidPathsStore for GatewayStoreQuery {
    #[tracing::instrument(level = "trace", skip_all, fields(path_count = paths.len(), substitute))]
    fn query_valid_paths(
        &mut self,
        paths: &[Vec<u8>],
        substitute: bool,
    ) -> io::Result<Vec<Vec<u8>>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = GatewayStoreConnection::connect(self.endpoint()?)?;
        connection.query_valid_paths(paths, substitute)
    }

    fn query_missing(&mut self, targets: &[Vec<u8>]) -> io::Result<MissingPaths> {
        let mut connection = GatewayStoreConnection::connect(self.endpoint()?)?;
        let missing = connection.query_missing(targets)?;
        Ok(MissingPaths {
            will_build: missing.will_build,
            will_substitute: missing.will_substitute,
            unknown: missing.unknown,
            download_size: missing.download_size,
            nar_size: missing.nar_size,
        })
    }
}
