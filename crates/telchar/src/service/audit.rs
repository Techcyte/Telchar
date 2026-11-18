//! Session-correlated, opt-in audit records for authenticated client operations.

use std::cell::Cell;
use std::time::Instant;

use serde_json::{Value, json};

use super::identity_details::IdentityDetails;
use super::ipc::RequesterMetadata;

pub struct SessionAudit<'a> {
    session_id: &'a str,
    requester: &'a RequesterMetadata,
    identity: Option<&'a IdentityDetails>,
    started: Instant,
    paths_uploaded: Cell<u64>,
    paths_rejected: Cell<u64>,
    builds_requested: Cell<u64>,
}

impl<'a> SessionAudit<'a> {
    pub fn new(
        session_id: &'a str,
        requester: &'a RequesterMetadata,
        identity: Option<&'a IdentityDetails>,
    ) -> Self {
        Self {
            session_id,
            requester,
            identity,
            started: Instant::now(),
            paths_uploaded: Cell::new(0),
            paths_rejected: Cell::new(0),
            builds_requested: Cell::new(0),
        }
    }

    pub fn started(&self, version: nix_worker_protocol::WorkerVersion) {
        let wire = version.to_wire();
        self.emit(
            "session.started",
            json!({
                "quota_subject": self.requester.quota_subject,
                "identity": self.identity,
                "client_protocol": format!("{}.{}", wire >> 8, wire & 255),
                "client_nix_version": null,
            }),
        );
    }

    pub fn upload(
        &self,
        info: &nix_worker_protocol::AddMultipleToStorePathInfo,
        already_valid: Option<bool>,
        result: &std::io::Result<()>,
    ) {
        let counter = if result.is_ok() {
            &self.paths_uploaded
        } else {
            &self.paths_rejected
        };
        counter.set(counter.get().saturating_add(1));
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let signatures = info
            .signatures()
            .iter()
            .map(|signature| {
                let signature = text(signature);
                let (key, value) = signature.split_once(':').unwrap_or(("", &signature));
                json!({"key_name": key, "value": value})
            })
            .collect::<Vec<_>>();
        self.emit(
            if result.is_ok() {
                "upload.accepted"
            } else {
                "upload.rejected"
            },
            json!({
                "reason": upload_reason(result),
                "store_path": text(info.path()),
                "nar_hash": text(info.nar_hash()),
                "nar_size": info.nar_size(),
                "content_address": info.content_address().map(text),
                "deriver": info.deriver().map(text),
                "reference_count": info.references().len(),
                "already_valid": already_valid,
                "signatures": signatures,
            }),
        );
    }

    pub fn build(&self, targets: Vec<String>) -> BuildAudit<'_, 'a> {
        self.builds_requested
            .set(self.builds_requested.get().saturating_add(1));
        self.emit("build.requested", json!({"targets": targets}));
        BuildAudit {
            session: self,
            targets,
            completed: Cell::new(false),
        }
    }

    pub fn emit(&self, event: &'static str, mut fields: Value) {
        fields["event"] = event.into();
        fields["session_id"] = self.session_id.into();
        fields["audit_subject"] = self.requester.audit_subject.as_str().into();
        fields["credential_id"] = self.requester.credential_id.as_str().into();
        telchar_telemetry::emit_audit(event, &fields.to_string());
    }
}

fn upload_reason(result: &std::io::Result<()>) -> &'static str {
    match result {
        Ok(()) => "accepted",
        Err(error) if error.to_string().contains("NAR hash mismatch") => "NAR hash mismatch",
        Err(error) if error.to_string().contains("NAR size mismatch") => "NAR size mismatch",
        Err(error) => crate::service::metrics::io_failure_class(error),
    }
}

pub struct BuildAudit<'s, 'a> {
    session: &'s SessionAudit<'a>,
    targets: Vec<String>,
    completed: Cell<bool>,
}

impl BuildAudit<'_, '_> {
    pub fn completed(&self, status: &str, outputs: Vec<String>) {
        self.completed.set(true);
        self.session.emit(
            "build.completed",
            json!({"targets": self.targets, "status": status, "output_paths": outputs}),
        );
    }
}

impl Drop for BuildAudit<'_, '_> {
    fn drop(&mut self) {
        if !self.completed.get() {
            self.session.emit(
                "build.completed",
                json!({"targets": self.targets, "status": "failed", "output_paths": []}),
            );
        }
    }
}

impl Drop for SessionAudit<'_> {
    fn drop(&mut self) {
        self.emit(
            "session.ended",
            json!({
                "duration_ms": self.started.elapsed().as_millis() as u64,
                "paths_uploaded": self.paths_uploaded.get(),
                "paths_rejected": self.paths_rejected.get(),
                "builds_requested": self.builds_requested.get(),
            }),
        );
    }
}
