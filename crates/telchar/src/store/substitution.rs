//! Realizes known gateway-store paths through the Nix daemon's configured substituters.

use std::io;

use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

pub trait StoreSubstitutionBackend: Send {
    fn ensure_path(&mut self, path: &[u8]) -> io::Result<()>;
}

pub struct GatewayStoreSubstitution {
    endpoint: GatewayStoreEndpoint,
}

impl GatewayStoreSubstitution {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self { endpoint }
    }
}

impl StoreSubstitutionBackend for GatewayStoreSubstitution {
    fn ensure_path(&mut self, path: &[u8]) -> io::Result<()> {
        GatewayStoreConnection::connect(&self.endpoint)?.ensure_path(path)
    }
}

pub struct UnavailableStoreSubstitution;

impl StoreSubstitutionBackend for UnavailableStoreSubstitution {
    fn ensure_path(&mut self, _path: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "gateway store is unavailable",
        ))
    }
}
