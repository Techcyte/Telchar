//! Captures gateway-store process configuration once and constructs explicit store dependencies.

use std::io;
use std::path::PathBuf;

use crate::backend::BuildBackend;
use crate::store::closure::{
    GatewayStoreClosureBackend, StoreClosureBackend, UnavailableStoreClosureBackend,
};
use crate::store::daemon::GatewayStoreEndpoint;
use crate::store::export::{
    GatewayStoreExportBackend, NixStoreExportBackend, StoreExportBackend,
    UnavailableStoreExportBackend,
};
use crate::store::import::{GatewayStoreImport, StoreImportBackend, UnavailableStoreImport};
use crate::store::query::GatewayStoreQuery;
use crate::store::retention::{
    backend_for_gateway_store, filesystem_backend, StoreRetentionBackend,
};

#[derive(Clone)]
pub struct GatewayStoreRuntime {
    endpoint: Option<GatewayStoreEndpoint>,
    nix_executable: String,
    environment: Vec<(String, String)>,
    build_helper: Option<PathBuf>,
    export_helper: Option<PathBuf>,
    gc_root_directory: Option<PathBuf>,
    filesystem_retention: bool,
}

impl GatewayStoreRuntime {
    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI")
            .map(|value| GatewayStoreEndpoint::parse_os(&value))
            .transpose()?;
        let environment = std::env::vars()
            .filter(|(name, _)| name.starts_with("NIX_") || name == "TMPDIR")
            .collect();
        Ok(Self {
            endpoint,
            nix_executable: std::env::var("TELCHAR_NIX").unwrap_or_else(|_| "nix".to_owned()),
            environment,
            build_helper: std::env::var_os("TELCHAR_TEST_BUILD_HELPER").map(PathBuf::from),
            export_helper: std::env::var_os("TELCHAR_TEST_EXPORT_HELPER").map(PathBuf::from),
            gc_root_directory: std::env::var_os("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY")
                .map(PathBuf::from),
            filesystem_retention: std::env::var_os("TELCHAR_TEST_STORE_RETENTION").is_some(),
        })
    }

    pub fn endpoint(&self) -> Option<&GatewayStoreEndpoint> {
        self.endpoint.as_ref()
    }

    pub fn build_helper(&self) -> Option<&std::path::Path> {
        self.build_helper.as_deref()
    }

    pub fn query(&self) -> GatewayStoreQuery {
        GatewayStoreQuery::with_endpoint_and_environment(
            self.nix_executable.clone(),
            self.endpoint.clone(),
            self.environment.clone(),
        )
    }

    pub fn export(&self) -> Box<dyn StoreExportBackend> {
        match (&self.export_helper, &self.endpoint) {
            (Some(helper), Some(endpoint)) => Box::new(NixStoreExportBackend::new(
                helper,
                endpoint.to_string(),
                [("TELCHAR_NIX".to_owned(), self.nix_executable.clone())],
            )),
            (None, Some(endpoint)) => Box::new(GatewayStoreExportBackend::new(endpoint.clone())),
            _ => Box::new(UnavailableStoreExportBackend),
        }
    }

    pub fn import(&self) -> io::Result<Box<dyn StoreImportBackend>> {
        match &self.endpoint {
            Some(endpoint) => Ok(Box::new(GatewayStoreImport::new(endpoint.clone())?)),
            None => Ok(Box::new(UnavailableStoreImport)),
        }
    }

    pub fn closure(&self) -> Box<dyn StoreClosureBackend> {
        match &self.endpoint {
            Some(endpoint) => Box::new(GatewayStoreClosureBackend::new(endpoint.clone())),
            None => Box::new(UnavailableStoreClosureBackend),
        }
    }

    pub fn retention(&self) -> io::Result<Box<dyn StoreRetentionBackend>> {
        let Some(root_directory) = &self.gc_root_directory else {
            return Ok(crate::store::retention::unavailable_backend());
        };
        if self.filesystem_retention {
            filesystem_backend(root_directory)
        } else if let Some(endpoint) = &self.endpoint {
            backend_for_gateway_store(endpoint.to_string(), root_directory)
        } else {
            Ok(crate::store::retention::unavailable_backend())
        }
    }

    pub fn build_executor(&self) -> io::Result<Box<dyn BuildBackend>> {
        match (&self.build_helper, &self.endpoint) {
            (Some(helper), Some(endpoint)) => Ok(Box::new(
                crate::backend::local::NixStoreExecutor::new(helper, endpoint.to_string())?,
            )),
            (None, Some(endpoint)) => Ok(Box::new(
                crate::backend::local::GatewayStoreExecutor::new(endpoint.clone()),
            )),
            _ => Ok(Box::new(crate::backend::local::UnavailableBuildExecutor)),
        }
    }
}
