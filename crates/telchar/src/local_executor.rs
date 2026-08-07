use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::build_request::BuildRequest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalExecutionRequest<'a> {
    request_id: &'a str,
    build: &'a BuildRequest,
    timeout: Duration,
}

impl<'a> LocalExecutionRequest<'a> {
    pub fn new(
        request_id: &'a str,
        build: &'a BuildRequest,
        timeout: Duration,
    ) -> io::Result<Self> {
        if request_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local execution request ID is empty",
            ));
        }
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local execution timeout is zero",
            ));
        }
        Ok(Self {
            request_id,
            build,
            timeout,
        })
    }

    pub fn request_id(&self) -> &str {
        self.request_id
    }

    pub fn build(&self) -> &BuildRequest {
        self.build
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalBuildStatus {
    Built,
    AlreadyValid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBuildResult {
    status: LocalBuildStatus,
    outputs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl LocalBuildResult {
    pub fn status(&self) -> LocalBuildStatus {
        self.status
    }

    pub fn outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.outputs
    }
}

pub struct NixStoreExecutor {
    helper: PathBuf,
    store_uri: String,
}

impl NixStoreExecutor {
    pub fn new(helper: impl Into<PathBuf>, store_uri: impl Into<String>) -> io::Result<Self> {
        let helper = helper.into();
        if !helper.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local executor helper path is not absolute",
            ));
        }
        let store_uri = store_uri.into();
        if store_uri.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gateway store endpoint is not configured",
            ));
        }
        Ok(Self { helper, store_uri })
    }

    pub fn helper(&self) -> &Path {
        &self.helper
    }

    pub fn store_uri(&self) -> &str {
        &self.store_uri
    }

    pub fn execute(
        &mut self,
        _request: &LocalExecutionRequest<'_>,
    ) -> io::Result<LocalBuildResult> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local BuildDerivation execution is unavailable",
        ))
    }
}
