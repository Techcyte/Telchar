use std::collections::BTreeMap;
use std::io::{self, Cursor, Read};
use std::time::SystemTime;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::nomad_backend::NomadClient;
use crate::nomad_transfer_authentication::{
    HmacCallbackVerifier, VerifiedHmacRequest, WorkloadIdentityVerifier,
};
use crate::nomad_transfer_protocol::{
    Authentication, Direction, FrameKind, ProtocolLimits, ProtocolSession, decode_metadata,
    read_frame,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallbackExecution {
    backend: String,
    namespace: String,
    job_id: String,
    shared_build_digest: String,
    task: String,
    build_request: Option<crate::build_request::BuildRequest>,
}

impl CallbackExecution {
    pub fn new(
        backend: String,
        namespace: String,
        job_id: String,
        shared_build_digest: String,
        task: String,
    ) -> io::Result<Self> {
        if [&backend, &namespace, &job_id, &shared_build_digest, &task]
            .into_iter()
            .any(|value| {
                value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
            })
        {
            return Err(invalid("Nomad callback execution identity is invalid"));
        }
        Ok(Self {
            backend,
            namespace,
            job_id,
            shared_build_digest,
            task,
            build_request: None,
        })
    }

    pub fn build_request(&self) -> Option<&crate::build_request::BuildRequest> {
        self.build_request.as_ref()
    }

    pub fn matches(&self, authentication: &Authentication) -> bool {
        authentication.backend == self.backend
            && authentication.namespace == self.namespace
            && authentication.job_id == self.job_id
            && authentication.shared_build_digest == self.shared_build_digest
            && authentication.task == self.task
    }
}

pub trait CallbackExecutionResolver {
    fn resolve(&self, authentication: &Authentication) -> io::Result<Option<CallbackExecution>>;
}

pub struct CallbackResolver<R> {
    resolver: R,
}

impl<R: CallbackExecutionResolver> CallbackResolver<R> {
    pub fn new(resolver: R) -> Self {
        Self { resolver }
    }

    pub fn resolve(
        &self,
        authentication: &Authentication,
    ) -> io::Result<Option<CallbackExecution>> {
        let execution = self.resolver.resolve(authentication)?;
        if execution
            .as_ref()
            .is_some_and(|execution| !execution.matches(authentication))
        {
            return Err(invalid("Nomad callback execution identity is inconsistent"));
        }
        Ok(execution)
    }
}

pub struct PostgresCallbackExecutionResolver {
    database_url: String,
    namespaces: BTreeMap<String, String>,
}

impl PostgresCallbackExecutionResolver {
    pub fn new(database_url: String, namespaces: Vec<(String, String)>) -> io::Result<Self> {
        if database_url.trim().is_empty() || namespaces.is_empty() {
            return Err(invalid("Nomad callback resolver configuration is invalid"));
        }
        let mut configured = BTreeMap::new();
        for (backend, namespace) in namespaces {
            if backend.is_empty()
                || namespace.is_empty()
                || configured.insert(backend, namespace).is_some()
            {
                return Err(invalid("Nomad callback resolver configuration is invalid"));
            }
        }
        Ok(Self {
            database_url,
            namespaces: configured,
        })
    }
}

impl CallbackExecutionResolver for PostgresCallbackExecutionResolver {
    fn resolve(&self, authentication: &Authentication) -> io::Result<Option<CallbackExecution>> {
        let Some(namespace) = self.namespaces.get(&authentication.backend) else {
            return Ok(None);
        };
        if namespace != &authentication.namespace {
            return Ok(None);
        }
        let build = crate::persistence::read_shared_build_by_execution(
            &self.database_url,
            &authentication.backend,
            &authentication.job_id,
        )
        .map_err(|_| io::Error::other("Nomad callback execution lookup failed"))?;
        let Some(build) = build else {
            return Ok(None);
        };
        if build.backend_kind != crate::backend::BackendKind::Nomad
            || !matches!(
                build.state,
                crate::persistence::SharedBuildState::Running
                    | crate::persistence::SharedBuildState::Collecting
            )
        {
            return Ok(None);
        }
        let request_digest = build
            .request_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let shared_build_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(format!(
            "{}:{request_digest}",
            build.derivation_path
        )));
        if authentication.shared_build_digest != shared_build_digest {
            return Ok(None);
        }
        let mut execution = CallbackExecution::new(
            build.backend_name,
            namespace.clone(),
            authentication.job_id.clone(),
            shared_build_digest,
            "build".to_owned(),
        )?;
        execution.build_request = build.build_request;
        Ok(Some(execution))
    }
}

pub trait AllocationVerifier {
    fn verify_allocation(&self, allocation_id: &str, job_id: &str, task: &str) -> io::Result<()>;
}

impl AllocationVerifier for NomadClient {
    fn verify_allocation(&self, allocation_id: &str, job_id: &str, task: &str) -> io::Result<()> {
        NomadClient::verify_allocation(self, allocation_id, job_id, task)
    }
}

pub enum AuthenticationVerification {
    WorkloadIdentity,
    Hmac(VerifiedHmacRequest),
}

pub trait AuthenticationVerifier {
    fn verify_authentication(
        &mut self,
        authentication: &Authentication,
        method: &str,
        path: &str,
        now: SystemTime,
    ) -> io::Result<AuthenticationVerification>;
}

impl AuthenticationVerifier for HmacCallbackVerifier {
    fn verify_authentication(
        &mut self,
        authentication: &Authentication,
        method: &str,
        path: &str,
        now: SystemTime,
    ) -> io::Result<AuthenticationVerification> {
        self.verify(authentication, method, path, now)
            .map(AuthenticationVerification::Hmac)
    }
}

impl AuthenticationVerifier for WorkloadIdentityVerifier {
    fn verify_authentication(
        &mut self,
        authentication: &Authentication,
        _method: &str,
        _path: &str,
        now: SystemTime,
    ) -> io::Result<AuthenticationVerification> {
        self.verify(authentication, now)?;
        Ok(AuthenticationVerification::WorkloadIdentity)
    }
}

pub trait ReplayAuthority {
    fn reserve(
        &self,
        authentication: &Authentication,
        verified: &VerifiedHmacRequest,
    ) -> io::Result<bool>;
}

pub struct PostgresReplayAuthority {
    database_url: String,
    maximum_retained_nonces: usize,
}

impl PostgresReplayAuthority {
    pub fn new(database_url: String, maximum_retained_nonces: usize) -> io::Result<Self> {
        if database_url.trim().is_empty() || maximum_retained_nonces == 0 {
            return Err(invalid("Nomad callback replay configuration is invalid"));
        }
        Ok(Self {
            database_url,
            maximum_retained_nonces,
        })
    }
}

impl ReplayAuthority for PostgresReplayAuthority {
    fn reserve(
        &self,
        authentication: &Authentication,
        verified: &VerifiedHmacRequest,
    ) -> io::Result<bool> {
        crate::persistence::reserve_nomad_callback_nonce(
            &self.database_url,
            &authentication.backend,
            &authentication.job_id,
            &authentication.allocation_id,
            verified.nonce(),
            verified.expires_at(),
            self.maximum_retained_nonces,
        )
        .map_err(|_| io::Error::other("Nomad callback replay persistence failed"))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessReplayAuthority;

impl ReplayAuthority for ProcessReplayAuthority {
    fn reserve(
        &self,
        _authentication: &Authentication,
        _verified: &VerifiedHmacRequest,
    ) -> io::Result<bool> {
        Ok(true)
    }
}

pub struct CallbackAdmission<A, V, R = ProcessReplayAuthority> {
    authentication: A,
    allocation: V,
    replay: R,
    maximum_metadata_bytes: usize,
}

impl<A: AuthenticationVerifier, V: AllocationVerifier>
    CallbackAdmission<A, V, ProcessReplayAuthority>
{
    pub fn new(authentication: A, allocation: V, maximum_metadata_bytes: usize) -> Self {
        Self {
            authentication,
            allocation,
            replay: ProcessReplayAuthority,
            maximum_metadata_bytes,
        }
    }
}

impl<A: AuthenticationVerifier, V: AllocationVerifier, R: ReplayAuthority>
    CallbackAdmission<A, V, R>
{
    pub fn with_replay(
        authentication: A,
        allocation: V,
        replay: R,
        maximum_metadata_bytes: usize,
    ) -> Self {
        Self {
            authentication,
            allocation,
            replay,
            maximum_metadata_bytes,
        }
    }

    pub fn admit(
        &mut self,
        body: &[u8],
        method: &str,
        path: &str,
        now: SystemTime,
    ) -> io::Result<AdmittedCallback> {
        let authentication = decode_authentication(body, self.maximum_metadata_bytes)?;
        let mut protocol = ProtocolSession::new();
        protocol.accept(Direction::WorkerToGateway, FrameKind::Authenticate)?;
        let verified =
            self.authentication
                .verify_authentication(&authentication, method, path, now)?;
        if let AuthenticationVerification::Hmac(verified) = &verified
            && !self.replay.reserve(&authentication, verified)?
        {
            return Err(invalid("Nomad callback request was replayed"));
        }
        self.allocation.verify_allocation(
            &authentication.allocation_id,
            &authentication.job_id,
            &authentication.task,
        )?;
        Ok(AdmittedCallback {
            authentication,
            protocol,
        })
    }

    pub fn allocation_verifier(&self) -> &V {
        &self.allocation
    }
}

pub struct AdmittedCallback {
    authentication: Authentication,
    protocol: ProtocolSession,
}

impl AdmittedCallback {
    pub fn authentication(&self) -> &Authentication {
        &self.authentication
    }

    pub fn protocol_mut(&mut self) -> &mut ProtocolSession {
        &mut self.protocol
    }
}

pub fn decode_authentication(
    body: &[u8],
    maximum_metadata_bytes: usize,
) -> io::Result<Authentication> {
    let mut input = Cursor::new(body);
    let frame = read_frame(&mut input, ProtocolLimits::new(maximum_metadata_bytes, 0))?;
    if input.read(&mut [0_u8; 1])? != 0 {
        return Err(invalid("Nomad callback contains trailing bytes"));
    }
    if frame.kind() != FrameKind::Authenticate || !frame.payload().is_empty() {
        return Err(invalid("Nomad callback authentication frame is invalid"));
    }
    decode_metadata::<Authentication>(frame.metadata(), maximum_metadata_bytes)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
