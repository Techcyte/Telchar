use std::io;
use std::time::Duration;

use crate::build_request::BuildRequest;

const MAXIMUM_REQUEST_ID_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildExecution<'a> {
    request_id: &'a str,
    build: &'a BuildRequest,
    timeout: Duration,
}

impl<'a> BuildExecution<'a> {
    pub fn new(
        request_id: &'a str,
        build: &'a BuildRequest,
        timeout: Duration,
    ) -> io::Result<Self> {
        if request_id.is_empty()
            || request_id.len() > MAXIMUM_REQUEST_ID_BYTES
            || request_id.contains('\0')
            || timeout.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build execution is invalid",
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
pub enum BuildStatus {
    Built,
    AlreadyValid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTrust {
    TrustedExecutor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildResult {
    status: BuildStatus,
    outputs: Vec<(Vec<u8>, Vec<u8>)>,
    output_trust: OutputTrust,
}

impl BuildResult {
    pub fn new(
        status: BuildStatus,
        outputs: Vec<(Vec<u8>, Vec<u8>)>,
        output_trust: OutputTrust,
    ) -> io::Result<Self> {
        if outputs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "build result has no outputs",
            ));
        }
        Ok(Self {
            status,
            outputs,
            output_trust,
        })
    }

    pub fn status(&self) -> BuildStatus {
        self.status
    }

    pub fn outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.outputs
    }

    pub fn output_trust(&self) -> OutputTrust {
        self.output_trust
    }
}

pub trait BuildBackend: Send {
    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult>;

    fn execute(&mut self, execution: &BuildExecution<'_>) -> io::Result<BuildResult> {
        self.execute_with_logs(execution, &mut |_| Ok(()), &mut || Ok(false))
    }
}
