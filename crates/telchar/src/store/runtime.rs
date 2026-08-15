//! Captures gateway-store process configuration once and constructs explicit store dependencies.

use std::io;
use std::path::PathBuf;

use crate::backend::BuildBackend;
use crate::store::closure::{
    GatewayStoreClosureBackend, StoreClosureBackend, UnavailableStoreClosureBackend,
};
use crate::store::daemon::GatewayStoreEndpoint;
#[cfg(debug_assertions)]
use crate::store::export::NixStoreExportBackend;
use crate::store::export::{
    GatewayStoreExportBackend, StoreExportBackend, UnavailableStoreExportBackend,
};
use crate::store::import::{GatewayStoreImport, StoreImportBackend, UnavailableStoreImport};
use crate::store::query::GatewayStoreQuery;
#[cfg(debug_assertions)]
use crate::store::retention::filesystem_backend;
use crate::store::retention::{backend_for_gateway_store, StoreRetentionBackend};
use crate::store::substitution::{
    GatewayStoreSubstitution, StoreSubstitutionBackend, UnavailableStoreSubstitution,
};

#[derive(Clone)]
pub struct GatewayStoreRuntime {
    endpoint: Option<GatewayStoreEndpoint>,
    nix_executable: String,
    environment: Vec<(String, String)>,
    #[cfg(debug_assertions)]
    build_helper: Option<PathBuf>,
    #[cfg(debug_assertions)]
    export_helper: Option<PathBuf>,
    gc_root_directory: Option<PathBuf>,
    #[cfg(debug_assertions)]
    filesystem_retention: bool,
}

impl GatewayStoreRuntime {
    pub fn from_environment() -> io::Result<Self> {
        let endpoint = std::env::var_os("TELCHAR_GATEWAY_STORE_URI")
            .map(|value| GatewayStoreEndpoint::parse_os(&value))
            .transpose()?;
        let environment = std::env::vars()
            .filter(|(name, _)| name.starts_with("NIX_") || name == "HOME" || name == "TMPDIR")
            .collect();
        Ok(Self {
            endpoint,
            nix_executable: std::env::var("TELCHAR_NIX").unwrap_or_else(|_| "nix".to_owned()),
            environment,
            #[cfg(debug_assertions)]
            build_helper: std::env::var_os("TELCHAR_TEST_BUILD_HELPER").map(PathBuf::from),
            #[cfg(debug_assertions)]
            export_helper: std::env::var_os("TELCHAR_TEST_EXPORT_HELPER").map(PathBuf::from),
            gc_root_directory: std::env::var_os("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY")
                .map(PathBuf::from),
            #[cfg(debug_assertions)]
            filesystem_retention: std::env::var_os("TELCHAR_TEST_STORE_RETENTION").is_some(),
        })
    }

    pub fn endpoint(&self) -> Option<&GatewayStoreEndpoint> {
        self.endpoint.as_ref()
    }

    pub fn build_helper(&self) -> Option<&std::path::Path> {
        #[cfg(debug_assertions)]
        {
            self.build_helper.as_deref()
        }
        #[cfg(not(debug_assertions))]
        None
    }

    pub fn query(&self) -> GatewayStoreQuery {
        GatewayStoreQuery::with_endpoint_and_environment(
            self.nix_executable.clone(),
            self.endpoint.clone(),
            self.environment.clone(),
        )
    }

    pub fn export(&self) -> Box<dyn StoreExportBackend> {
        #[cfg(debug_assertions)]
        if let (Some(helper), Some(endpoint)) = (&self.export_helper, &self.endpoint) {
            return Box::new(NixStoreExportBackend::new(
                helper,
                endpoint.to_string(),
                [("TELCHAR_NIX".to_owned(), self.nix_executable.clone())],
            ));
        }
        match &self.endpoint {
            Some(endpoint) => Box::new(GatewayStoreExportBackend::new(endpoint.clone())),
            None => Box::new(UnavailableStoreExportBackend),
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

    pub fn substitution(&self) -> Box<dyn StoreSubstitutionBackend> {
        #[cfg(debug_assertions)]
        if self.build_helper.is_some() {
            return Box::new(UnavailableStoreSubstitution);
        }
        match &self.endpoint {
            Some(endpoint) => Box::new(GatewayStoreSubstitution::new(endpoint.clone())),
            None => Box::new(UnavailableStoreSubstitution),
        }
    }

    pub fn retention(&self) -> io::Result<Box<dyn StoreRetentionBackend>> {
        let Some(root_directory) = &self.gc_root_directory else {
            return Ok(crate::store::retention::unavailable_backend());
        };
        #[cfg(debug_assertions)]
        if self.filesystem_retention {
            return filesystem_backend(root_directory);
        }
        if let Some(endpoint) = &self.endpoint {
            backend_for_gateway_store(endpoint.to_string(), root_directory)
        } else {
            Ok(crate::store::retention::unavailable_backend())
        }
    }

    pub fn build_executor(&self) -> io::Result<Box<dyn BuildBackend>> {
        #[cfg(debug_assertions)]
        if let (Some(helper), Some(endpoint)) = (&self.build_helper, &self.endpoint) {
            return Ok(Box::new(crate::backend::local::NixStoreExecutor::new(
                helper,
                endpoint.to_string(),
            )?));
        }
        match &self.endpoint {
            Some(endpoint) => Ok(Box::new(crate::backend::local::GatewayStoreExecutor::new(
                endpoint.clone(),
            ))),
            None => Ok(Box::new(crate::backend::local::UnavailableBuildExecutor)),
        }
    }
}
