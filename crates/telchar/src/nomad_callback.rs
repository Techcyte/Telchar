use std::io::{self, Cursor, Read};
use std::time::SystemTime;

use crate::nomad_backend::NomadClient;
use crate::nomad_transfer_authentication::{
    HmacCallbackVerifier, VerifiedHmacRequest, WorkloadIdentityVerifier,
};
use crate::nomad_transfer_protocol::{
    Authentication, Direction, FrameKind, ProtocolLimits, ProtocolSession, decode_metadata,
    read_frame,
};

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
        let mut input = Cursor::new(body);
        let frame = read_frame(
            &mut input,
            ProtocolLimits::new(self.maximum_metadata_bytes, 0),
        )?;
        if input.read(&mut [0_u8; 1])? != 0 {
            return Err(invalid("Nomad callback contains trailing bytes"));
        }
        if frame.kind() != FrameKind::Authenticate || !frame.payload().is_empty() {
            return Err(invalid("Nomad callback authentication frame is invalid"));
        }
        let authentication =
            decode_metadata::<Authentication>(frame.metadata(), self.maximum_metadata_bytes)?;
        let mut protocol = ProtocolSession::new();
        protocol.accept(Direction::WorkerToGateway, FrameKind::Authenticate)?;
        let verified = self
            .authentication
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
