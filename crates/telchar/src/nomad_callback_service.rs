use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::config::{NomadBackendConfig, NomadCallbackConfig, NomadTransferAuthentication};
use crate::nomad_callback::{
    decode_authentication, CallbackAdmission, CallbackResolver, PostgresCallbackExecutionResolver,
    PostgresReplayAuthority,
};
use crate::nomad_callback_http::{accept_connection, CallbackHttpLimits};
use crate::nomad_transfer_authentication::{
    HmacCallbackVerifier, HmacVerificationPolicy, WorkloadIdentityPolicy, WorkloadIdentityVerifier,
};
use crate::nomad_transfer_protocol::{
    encode_metadata, write_frame, BuildSpecification, Frame, FrameKind, InputManifest,
    PathManifestEntry, ProtocolLimits, TransferSession,
};
use crate::store_closure::{backend_from_environment, StoreClosureBackend};

pub fn serve(
    listener: TcpListener,
    callback: NomadCallbackConfig,
    database_url: String,
    backends: Vec<NomadBackendConfig>,
) -> io::Result<()> {
    let active = Arc::new(Mutex::new(0_usize));
    for connection in listener.incoming() {
        let mut connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(
                    event = "nomad.callback.connection_rejected",
                    reason = error.kind().to_string(),
                    "Nomad callback connection rejected"
                );
                continue;
            }
        };
        let Some(permit) =
            ConnectionPermit::acquire(Arc::clone(&active), callback.maximum_connections())
        else {
            tracing::warn!(
                event = "nomad.callback.connection_rejected",
                reason = "capacity"
            );
            continue;
        };
        let callback = callback.clone();
        let database_url = database_url.clone();
        let backends = backends.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            if let Err(error) =
                serve_connection(&mut connection, &callback, &database_url, &backends)
            {
                tracing::warn!(
                    event = "nomad.callback.connection_failed",
                    reason = error.kind().to_string(),
                    "Nomad callback connection failed"
                );
            }
        });
    }
    Ok(())
}

pub fn serve_connection(
    connection: &mut TcpStream,
    callback: &NomadCallbackConfig,
    database_url: &str,
    backends: &[NomadBackendConfig],
) -> io::Result<()> {
    let namespaces = backends
        .iter()
        .map(|backend| {
            (
                backend.target().name().to_owned(),
                backend.namespace().to_owned(),
            )
        })
        .collect();
    let resolver = CallbackResolver::new(PostgresCallbackExecutionResolver::new(
        database_url.to_owned(),
        namespaces,
    )?);
    let limits = CallbackHttpLimits::new(
        callback.maximum_header_bytes(),
        callback.maximum_body_bytes(),
    );
    let mut socket = accept_connection(connection, limits)?;
    let body = socket.read_binary()?;
    let authentication = decode_authentication(&body, callback.maximum_body_bytes())?;
    let execution = resolver
        .resolve(&authentication)?
        .ok_or_else(|| io::Error::other("Nomad callback execution is unavailable"))?;
    let backend = backends
        .iter()
        .find(|backend| backend.target().name() == authentication.backend)
        .ok_or_else(|| io::Error::other("Nomad callback backend is unavailable"))?;
    let allocation = crate::nomad_backend::NomadClient::new(backend.clone())?;
    match backend.transfer_authentication() {
        NomadTransferAuthentication::WorkloadIdentity {
            issuer,
            jwks_url,
            audience,
            ca_certificate_file,
        } => {
            let verifier = WorkloadIdentityVerifier::new(WorkloadIdentityPolicy {
                issuer: issuer.clone(),
                jwks_url: jwks_url.clone(),
                audience: audience.clone(),
                namespace: backend.namespace().to_owned(),
                job_id: authentication.job_id.clone(),
                task: "build".to_owned(),
                ca_certificate_file: ca_certificate_file.clone(),
                request_timeout: callback.authentication_request_timeout(),
                maximum_jwks_bytes: callback.maximum_jwks_bytes(),
                clock_skew: backend.transfer_limits().clock_skew(),
            })?;
            CallbackAdmission::new(
                verifier,
                allocation,
                backend.transfer_limits().maximum_frame_metadata_bytes(),
            )
            .admit(
                &body,
                "GET",
                callback_path(callback.public_url())?,
                SystemTime::now(),
            )?;
        }
        NomadTransferAuthentication::Hmac {
            key_id,
            secret_file,
        } => {
            let secret = std::fs::read(secret_file)
                .map_err(|_| io::Error::other("Nomad callback secret could not be read"))?;
            let verifier = HmacCallbackVerifier::new(HmacVerificationPolicy {
                key_id: key_id.clone(),
                secret,
                backend: authentication.backend.clone(),
                namespace: backend.namespace().to_owned(),
                job_id: authentication.job_id.clone(),
                shared_build_digest: authentication.shared_build_digest.clone(),
                task: "build".to_owned(),
                clock_skew: backend.transfer_limits().clock_skew(),
                nonce_retention: backend.transfer_limits().nonce_retention(),
                maximum_retained_nonces: callback.maximum_retained_nonces(),
            })?;
            let replay = PostgresReplayAuthority::new(
                database_url.to_owned(),
                callback.maximum_retained_nonces(),
            )?;
            CallbackAdmission::with_replay(
                verifier,
                allocation,
                replay,
                backend.transfer_limits().maximum_frame_metadata_bytes(),
            )
            .admit(
                &body,
                "GET",
                callback_path(callback.public_url())?,
                SystemTime::now(),
            )?;
        }
    }
    if !execution.matches(&authentication) {
        return Err(io::Error::other(
            "Nomad callback execution identity is inconsistent",
        ));
    }
    let build_request = execution
        .build_request()
        .ok_or_else(|| io::Error::other("Nomad callback build specification is unavailable"))?;
    let limits = backend.transfer_limits();
    let mut closure = backend_from_environment()?;
    let manifest = input_manifest(build_request, closure.as_mut())?;
    let _session = TransferSession::new(
        manifest.clone(),
        limits.maximum_manifest_paths(),
        limits.maximum_input_nar_bytes(),
        limits.maximum_total_input_bytes(),
        limits.maximum_output_nar_bytes(),
        limits.maximum_total_output_bytes(),
        limits.maximum_live_log_chunk_bytes(),
        limits.maximum_frame_metadata_bytes(),
    )?;
    let frame = Frame::new(
        FrameKind::InputManifest,
        encode_metadata(&manifest, limits.maximum_frame_metadata_bytes())?,
        Vec::new(),
    );
    let mut message = Vec::new();
    write_frame(
        &mut message,
        &frame,
        ProtocolLimits::new(limits.maximum_frame_metadata_bytes(), 0),
    )?;
    socket.write_binary(message)
}

fn input_manifest(
    build_request: &crate::build_request::BuildRequest,
    closure: &mut dyn StoreClosureBackend,
) -> io::Result<InputManifest> {
    build_request.validate_for_execution()?;
    let mut roots = build_request.input_sources().to_vec();
    roots.push(build_request.derivation_path().to_vec());
    let closure_paths = closure.input_closure(&roots)?;
    let closure_identities = closure_paths
        .iter()
        .map(|path| path.store_path.as_bytes())
        .collect::<std::collections::BTreeSet<_>>();
    if !roots
        .iter()
        .all(|root| closure_identities.contains(root.as_slice()))
    {
        return Err(io::Error::other(
            "Nomad callback input closure is incomplete",
        ));
    }
    let paths = closure_paths
        .into_iter()
        .map(|path| PathManifestEntry {
            path: path.store_path,
            nar_hash: path.nar_hash,
            nar_size: path.nar_size,
            references: path.references,
            deriver: path.deriver,
        })
        .collect();
    let derivation_path = std::str::from_utf8(build_request.derivation_path())
        .map_err(|_| io::Error::other("Nomad callback derivation path is invalid"))?
        .to_owned();
    let outputs = build_request
        .expected_outputs()
        .iter()
        .map(|(_, path)| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|_| io::Error::other("Nomad callback output path is invalid"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(InputManifest {
        derivation_path,
        build: BuildSpecification::from(build_request),
        paths,
        outputs,
    })
}

fn callback_path(public_url: &str) -> io::Result<&str> {
    let scheme = public_url
        .find("://")
        .ok_or_else(|| io::Error::other("Nomad callback URL is invalid"))?;
    let authority = &public_url[scheme + 3..];
    Ok(authority
        .find('/')
        .map_or("/", |offset| &authority[offset..]))
}

struct ConnectionPermit {
    active: Arc<Mutex<usize>>,
}

impl ConnectionPermit {
    fn acquire(active: Arc<Mutex<usize>>, maximum: usize) -> Option<Self> {
        let mut count = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *count >= maximum {
            return None;
        }
        *count += 1;
        drop(count);
        Some(Self { active })
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut count = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *count = count.saturating_sub(1);
    }
}
