use std::io::{self, Cursor, Read};
use std::time::SystemTime;

use crate::nomad_backend::NomadClient;
use crate::nomad_transfer_authentication::HmacCallbackVerifier;
use crate::nomad_transfer_protocol::{
    decode_metadata, read_frame, Authentication, Direction, FrameKind, ProtocolLimits,
    ProtocolSession,
};

pub trait AllocationVerifier {
    fn verify_allocation(&self, allocation_id: &str, job_id: &str, task: &str) -> io::Result<()>;
}

impl AllocationVerifier for NomadClient {
    fn verify_allocation(&self, allocation_id: &str, job_id: &str, task: &str) -> io::Result<()> {
        NomadClient::verify_allocation(self, allocation_id, job_id, task)
    }
}

pub struct CallbackAdmission<V> {
    hmac: HmacCallbackVerifier,
    allocation: V,
    maximum_metadata_bytes: usize,
}

impl<V: AllocationVerifier> CallbackAdmission<V> {
    pub fn new(hmac: HmacCallbackVerifier, allocation: V, maximum_metadata_bytes: usize) -> Self {
        Self {
            hmac,
            allocation,
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
        self.hmac.verify(&authentication, method, path, now)?;
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
