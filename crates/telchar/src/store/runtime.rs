//! Captures gateway-store process configuration once and constructs explicit store dependencies.

use std::io;
use std::path::PathBuf;

use crate::backend::BuildBackend;
use crate::store::closure::{GatewayStoreClosureBackend, StoreClosureBackend};
use crate::store::daemon::GatewayStoreEndpoint;
use crate::store::export::{GatewayStoreExportBackend, NixStoreExportBackend, StoreExportBackend};
use crate::store::import::{GatewayStoreImport, StoreImportBackend};
use crate::store::query::GatewayStoreQuery;
use crate::store::retention::{
    backend_for_gateway_store, filesystem_backend, StoreRetentionBackend,
};

#[derive(Clone)]
pub struct GatewayStoreRuntime {
    endpoint: GatewayStoreEndpoint,
    nix_executable: String,
    build_helper: Option<PathBuf>,
    export_helper: Option<PathBuf>,
    gc_root_directory: Option<PathBuf>,
    filesystem_retention: bool,
}

impl GatewayStoreRuntime {
    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "gateway store endpoint is not configured",
                )
            })
            .and_then(|value| GatewayStoreEndpoint::parse_os(&value))?;
        Ok(Self {
            endpoint,
            nix_executable: std::env::var("TELCHAR_NIX").unwrap_or_else(|_| "nix".to_owned()),
            build_helper: std::env::var_os("TELCHAR_TEST_BUILD_HELPER").map(PathBuf::from),
            export_helper: std::env::var_os("TELCHAR_TEST_EXPORT_HELPER").map(PathBuf::from),
            gc_root_directory: std::env::var_os("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY")
                .map(PathBuf::from),
            filesystem_retention: std::env::var_os("TELCHAR_TEST_STORE_RETENTION").is_some(),
        })
    }

    pub fn endpoint(&self) -> &GatewayStoreEndpoint {
        &self.endpoint
    }

    pub fn query(&self) -> GatewayStoreQuery {
        GatewayStoreQuery::new(self.nix_executable.clone(), self.endpoint.clone())
    }

    pub fn export(&self) -> Box<dyn StoreExportBackend> {
        match &self.export_helper {
            Some(helper) => Box::new(NixStoreExportBackend::new(
                helper,
                self.endpoint.to_string(),
                [("TELCHAR_NIX".to_owned(), self.nix_executable.clone())],
            )),
            None => Box::new(GatewayStoreExportBackend::new(self.endpoint.clone())),
        }
    }

    pub fn import(&self) -> io::Result<Box<dyn StoreImportBackend>> {
        Ok(Box::new(GatewayStoreImport::new(self.endpoint.clone())?))
    }

    pub fn closure(&self) -> Box<dyn StoreClosureBackend> {
        Box::new(GatewayStoreClosureBackend::new(self.endpoint.clone()))
    }

    pub fn retention(&self) -> io::Result<Box<dyn StoreRetentionBackend>> {
        let Some(root_directory) = &self.gc_root_directory else {
            return Ok(crate::store::retention::unavailable_backend());
        };
        if self.filesystem_retention {
            filesystem_backend(root_directory)
        } else {
            backend_for_gateway_store(self.endpoint.to_string(), root_directory)
        }
    }

    pub fn build_executor(&self) -> io::Result<Box<dyn BuildBackend>> {
        match &self.build_helper {
            Some(helper) => Ok(Box::new(crate::backend::local::NixStoreExecutor::new(
                helper,
                self.endpoint.to_string(),
            )?)),
            None => Ok(Box::new(crate::backend::local::GatewayStoreExecutor::new(
                self.endpoint.clone(),
            ))),
        }
    }
}
