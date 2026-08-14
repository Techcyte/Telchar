//! Owns PostgreSQL migrations and durable request, lease, shared-build, attempt, transfer, and recovery operations.

use std::fmt;

use std::time::{Duration, SystemTime};

use postgres::{Client, NoTls, Row};
use sha2::{Digest, Sha256};

use crate::backend::{
    BackendCapabilities, BackendKind, CancellationCapability, ExecutionRecovery, LogRecovery,
};
use crate::ipc::{RequesterMetadata, MAX_IPC_COMPONENT_BYTES};

const MIGRATION_LOCK_KEY: i64 = 0x5445_4c43_4841_5201_u64 as i64;
const RETAINED_INPUT_ADMISSION_LOCK_KEY: i64 = 0x5445_4c43_4841_5204_u64 as i64;

pub fn requester_reference(requester: &RequesterMetadata) -> String {
    let mut digest = Sha256::new();
    digest.update(b"telchar-requester-v1\0");
    digest.update(requester.credential_id.as_bytes());
    digest.update(b"\0");
    digest.update(requester.audit_subject.as_bytes());
    digest.update(b"\0");
    digest.update(requester.quota_subject.as_bytes());
    format!("{:x}", digest.finalize())
}

mod attachments;
mod build_requests;
mod callback_nonces;
mod executor;
mod leases;
mod migrations;
mod sessions;
mod shared_builds;

pub use attachments::*;
pub use build_requests::*;
pub use callback_nonces::*;
pub use executor::*;
pub use leases::*;
pub use migrations::*;
pub use sessions::*;
pub use shared_builds::*;
