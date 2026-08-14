use super::*;

pub(super) fn validate_nomad_callback(
    raw: RawNomadCallbackConfig,
) -> io::Result<NomadCallbackConfig> {
    let bind = raw
        .bind
        .as_deref()
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_BIND)
        .parse::<SocketAddr>()
        .map_err(|_| invalid("Nomad callback bind address is invalid"))?;
    let public_url = raw
        .public_url
        .unwrap_or_else(|| DEFAULT_NOMAD_CALLBACK_PUBLIC_URL.to_owned());
    if !valid_nomad_transfer_endpoint(&public_url) {
        return Err(invalid("Nomad callback public URL is invalid"));
    }
    let maximum_connections = raw
        .maximum_connections
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_MAXIMUM_CONNECTIONS);
    let maximum_header_bytes = raw
        .maximum_header_bytes
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_MAXIMUM_HEADER_BYTES);
    let maximum_body_bytes = raw
        .maximum_body_bytes
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_MAXIMUM_BODY_BYTES);
    let authentication_request_timeout_seconds = raw
        .authentication_request_timeout_seconds
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_AUTHENTICATION_REQUEST_TIMEOUT_SECONDS);
    let shutdown_drain_timeout_seconds = raw
        .shutdown_drain_timeout_seconds
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_SHUTDOWN_DRAIN_TIMEOUT_SECONDS);
    let maximum_jwks_bytes = raw
        .maximum_jwks_bytes
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_MAXIMUM_JWKS_BYTES);
    let maximum_retained_nonces = raw
        .maximum_retained_nonces
        .unwrap_or(DEFAULT_NOMAD_CALLBACK_MAXIMUM_RETAINED_NONCES);
    if maximum_connections == 0
        || maximum_connections > MAXIMUM_NOMAD_CALLBACK_CONNECTIONS
        || maximum_header_bytes == 0
        || maximum_header_bytes > MAXIMUM_NOMAD_CALLBACK_HEADER_BYTES
        || maximum_body_bytes == 0
        || maximum_body_bytes > MAXIMUM_NOMAD_CALLBACK_BODY_BYTES
        || authentication_request_timeout_seconds == 0
        || authentication_request_timeout_seconds > MAXIMUM_NOMAD_TRANSFER_TIMEOUT_SECONDS
        || shutdown_drain_timeout_seconds == 0
        || shutdown_drain_timeout_seconds > MAXIMUM_NOMAD_TRANSFER_TIMEOUT_SECONDS
        || maximum_jwks_bytes == 0
        || maximum_jwks_bytes > MAXIMUM_NOMAD_CALLBACK_BODY_BYTES
        || maximum_retained_nonces == 0
        || maximum_retained_nonces > MAXIMUM_NOMAD_CALLBACK_CONNECTIONS * 1024
    {
        return Err(invalid("Nomad callback service limits are invalid"));
    }
    Ok(NomadCallbackConfig {
        bind,
        public_url,
        maximum_connections,
        maximum_header_bytes,
        maximum_body_bytes,
        authentication_request_timeout: Duration::from_secs(authentication_request_timeout_seconds),
        shutdown_drain_timeout: Duration::from_secs(shutdown_drain_timeout_seconds),
        maximum_jwks_bytes,
        maximum_retained_nonces,
    })
}

pub(super) fn validate_mappings(
    raw: BTreeMap<String, RawCredentialMapping>,
) -> io::Result<BTreeMap<String, CredentialMapping>> {
    if raw.len() > MAXIMUM_CREDENTIAL_MAPPINGS {
        return Err(invalid("credential mapping count exceeds limit"));
    }
    raw.into_iter()
        .map(|(credential_id, mapping)| {
            if credential_id.is_empty()
                || credential_id.len() > MAXIMUM_CREDENTIAL_ID_BYTES
                || !(credential_id.starts_with("ssh-pubkey:")
                    || credential_id.starts_with("ssh-cert:"))
            {
                return Err(invalid("credential mapping ID is invalid"));
            }
            if credential_id == "ssh-pubkey:" || credential_id == "ssh-cert:" {
                return Err(invalid("credential mapping ID is invalid"));
            }
            let audit_subject = mapping
                .audit_subject
                .map(|value| validate_subject(value, "audit subject is invalid"))
                .transpose()?;
            let quota_subject = mapping
                .quota_subject
                .map(|value| validate_subject(value, "quota subject is invalid"))
                .transpose()?;
            if audit_subject.is_none() && quota_subject.is_none() {
                return Err(invalid("credential mapping is empty"));
            }
            Ok((
                credential_id,
                CredentialMapping {
                    audit_subject,
                    quota_subject,
                },
            ))
        })
        .collect()
}

pub(super) fn validate_scheduling_limits(raw: RawSchedulingLimits) -> io::Result<SchedulingLimits> {
    SchedulingLimits::new(raw.maximum_queued_builds, raw.maximum_active_builds)
}

pub(super) fn validate_local_backend(raw: RawLocalBackendConfig) -> io::Result<LocalBackendConfig> {
    validate_backend_capacity(raw.maximum_concurrent_builds)?;
    Ok(LocalBackendConfig {
        target: BackendTarget::new(
            &raw.name,
            BackendKind::Local,
            &raw.system,
            &raw.supported_features,
        )?,
        maximum_concurrent_builds: raw.maximum_concurrent_builds,
    })
}

pub(super) fn validate_static_ssh_backends(
    raw: Vec<RawStaticSshBackendConfig>,
) -> io::Result<Vec<StaticSshBackendConfig>> {
    if raw.len() > MAXIMUM_STATIC_SSH_BACKENDS {
        return Err(invalid("static SSH backend count exceeds limit"));
    }
    let mut backends = Vec::with_capacity(raw.len());
    for backend in raw {
        if backends
            .iter()
            .any(|existing: &StaticSshBackendConfig| existing.target.name() == backend.name)
        {
            return Err(invalid("static SSH backend name is ambiguous"));
        }
        validate_backend_capacity(backend.maximum_concurrent_builds)?;
        if !valid_ssh_destination(&backend.destination) {
            return Err(invalid("static SSH destination is invalid"));
        }
        validate_identity_file(&backend.identity_file)?;
        validate_known_hosts_file(&backend.known_hosts_file)?;
        let ssh_program = backend
            .ssh_program
            .unwrap_or_else(|| PathBuf::from(PACKAGED_SSH_PROGRAM.unwrap_or(SYSTEM_SSH_PROGRAM)));
        validate_executable_file(&ssh_program, "static SSH program is invalid")?;
        backends.push(StaticSshBackendConfig {
            target: BackendTarget::new(
                &backend.name,
                BackendKind::StaticSsh,
                &backend.system,
                &backend.supported_features,
            )?,
            maximum_concurrent_builds: backend.maximum_concurrent_builds,
            destination: backend.destination,
            identity_file: backend.identity_file,
            known_hosts_file: backend.known_hosts_file,
            ssh_program,
        });
    }
    Ok(backends)
}

pub(super) fn validate_nomad_backends(
    raw: Vec<RawNomadBackendConfig>,
    default_transfer_endpoint: &str,
) -> io::Result<Vec<NomadBackendConfig>> {
    if raw.len() > MAXIMUM_NOMAD_BACKENDS {
        return Err(invalid("Nomad backend count exceeds limit"));
    }
    let mut backends = Vec::with_capacity(raw.len());
    for backend in raw {
        if backends
            .iter()
            .any(|existing: &NomadBackendConfig| existing.target.name() == backend.name)
        {
            return Err(invalid("Nomad backend name is ambiguous"));
        }
        validate_backend_capacity(backend.maximum_concurrent_builds)?;
        if !valid_nomad_endpoint(&backend.endpoint) {
            return Err(invalid("Nomad endpoint is invalid"));
        }
        let namespace = validate_subject(backend.namespace, "Nomad namespace is invalid")?;
        let driver = validate_subject(backend.driver, "Nomad task driver is invalid")?;
        let job_name_scope =
            validate_subject(backend.job_name_scope, "Nomad job-name scope is invalid")?;
        if backend.poll_interval_seconds == 0
            || backend.poll_interval_seconds > MAXIMUM_NOMAD_POLL_INTERVAL_SECONDS
            || backend.runtime_limit_seconds == 0
            || backend.runtime_limit_seconds > MAXIMUM_NOMAD_RUNTIME_LIMIT_SECONDS
            || backend.poll_interval_seconds > backend.runtime_limit_seconds
        {
            return Err(invalid("Nomad timing bounds are invalid"));
        }
        let resources = validate_nomad_resources(backend.resources)?;
        let token_file = backend
            .token_file
            .map(|path| validate_protected_file(path, "Nomad token file is invalid"))
            .transpose()?;
        let ca_certificate_file = backend
            .ca_certificate_file
            .map(|path| validate_public_file(path, "Nomad CA certificate file is invalid"))
            .transpose()?;
        let client_certificate_file = backend
            .client_certificate_file
            .map(|path| validate_public_file(path, "Nomad client certificate file is invalid"))
            .transpose()?;
        let client_key_file = backend
            .client_key_file
            .map(|path| validate_protected_file(path, "Nomad client key file is invalid"))
            .transpose()?;
        if client_certificate_file.is_some() != client_key_file.is_some() {
            return Err(invalid(
                "Nomad client certificate and key must be configured together",
            ));
        }
        let driver_config = validate_driver_config(backend.driver_config)?;
        let transfer_endpoint = backend
            .transfer_endpoint
            .unwrap_or_else(|| default_transfer_endpoint.to_owned());
        if !valid_nomad_transfer_endpoint(&transfer_endpoint) {
            return Err(invalid("Nomad transfer endpoint is invalid"));
        }
        let transfer_authentication =
            validate_nomad_transfer_authentication(backend.transfer_authentication)?;
        let store = validate_nomad_store(backend.store)?;
        let transfer_limits = validate_nomad_transfer_limits(backend.transfer_limits)?;
        let prestart = backend.prestart.map(validate_nomad_prestart).transpose()?;
        backends.push(NomadBackendConfig {
            target: BackendTarget::new(
                &backend.name,
                BackendKind::Nomad,
                &backend.system,
                &backend.supported_features,
            )?,
            maximum_concurrent_builds: backend.maximum_concurrent_builds,
            endpoint: backend.endpoint,
            namespace,
            token_file,
            ca_certificate_file,
            client_certificate_file,
            client_key_file,
            driver,
            driver_config,
            resources,
            job_name_scope,
            poll_interval: Duration::from_secs(backend.poll_interval_seconds),
            runtime_limit: Duration::from_secs(backend.runtime_limit_seconds),
            transfer_endpoint,
            transfer_authentication,
            store,
            transfer_limits,
            prestart,
        });
    }
    Ok(backends)
}

pub(super) fn validate_nomad_transfer_authentication(
    raw: RawNomadTransferAuthentication,
) -> io::Result<NomadTransferAuthentication> {
    match raw {
        RawNomadTransferAuthentication::WorkloadIdentity {
            issuer,
            jwks_url,
            audience,
            ca_certificate_file,
        } => {
            if !valid_nomad_endpoint(&issuer) || !valid_nomad_endpoint(&jwks_url) {
                return Err(invalid("Nomad workload identity endpoint is invalid"));
            }
            let audience =
                validate_subject(audience, "Nomad workload identity audience is invalid")?;
            let ca_certificate_file = ca_certificate_file
                .map(|path| {
                    validate_public_file(path, "Nomad workload identity CA file is invalid")
                })
                .transpose()?;
            Ok(NomadTransferAuthentication::WorkloadIdentity {
                issuer,
                jwks_url,
                audience,
                ca_certificate_file,
            })
        }
        RawNomadTransferAuthentication::Hmac {
            key_id,
            secret_file,
        } => Ok(NomadTransferAuthentication::Hmac {
            key_id: validate_subject(key_id, "Nomad transfer HMAC key ID is invalid")?,
            secret_file: validate_protected_file(
                secret_file,
                "Nomad transfer HMAC secret file is invalid",
            )?,
        }),
    }
}

pub(super) fn validate_nomad_store(raw: RawNomadStoreConfig) -> io::Result<NomadStoreConfig> {
    match raw {
        RawNomadStoreConfig::Daemon { uri } => {
            if uri.is_empty()
                || uri.len() > MAXIMUM_NOMAD_STORE_URI_BYTES
                || uri
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            {
                return Err(invalid("Nomad store URI is invalid"));
            }
            Ok(NomadStoreConfig { uri })
        }
    }
}

pub(super) fn validate_nomad_transfer_limits(
    raw: RawNomadTransferLimits,
) -> io::Result<NomadTransferLimits> {
    if raw.maximum_manifest_paths == 0
        || raw.maximum_manifest_paths > MAXIMUM_NOMAD_TRANSFER_PATHS
        || !valid_transfer_bytes(raw.maximum_manifest_bytes)
        || !valid_transfer_bytes(raw.maximum_input_nar_bytes)
        || !valid_transfer_bytes(raw.maximum_total_input_bytes)
        || !valid_transfer_bytes(raw.maximum_output_nar_bytes)
        || !valid_transfer_bytes(raw.maximum_total_output_bytes)
        || raw.maximum_input_nar_bytes > raw.maximum_total_input_bytes
        || raw.maximum_output_nar_bytes > raw.maximum_total_output_bytes
        || !valid_transfer_memory(raw.maximum_frame_metadata_bytes)
        || !valid_transfer_memory(raw.stream_buffer_bytes)
        || !valid_transfer_memory(raw.maximum_live_log_chunk_bytes)
        || !valid_transfer_memory(raw.live_log_queue_bytes)
        || raw.maximum_live_log_chunk_bytes > raw.live_log_queue_bytes
        || !valid_transfer_timeout(raw.transfer_idle_timeout_seconds)
        || !valid_transfer_timeout(raw.setup_timeout_seconds)
        || !valid_transfer_timeout(raw.output_collection_timeout_seconds)
        || !valid_transfer_timeout(raw.maximum_connection_lifetime_seconds)
        || raw.authentication_lifetime_seconds == 0
        || raw.authentication_lifetime_seconds > MAXIMUM_NOMAD_AUTHENTICATION_SECONDS
        || raw.clock_skew_seconds > MAXIMUM_NOMAD_AUTHENTICATION_SECONDS
        || raw.nonce_retention_seconds == 0
        || raw.nonce_retention_seconds > MAXIMUM_NOMAD_NONCE_RETENTION_SECONDS
        || raw.nonce_retention_seconds
            < raw.authentication_lifetime_seconds + raw.clock_skew_seconds
        || !valid_transfer_timeout(raw.reconnect_timeout_seconds)
        || !valid_transfer_memory(raw.maximum_diagnostic_bytes)
    {
        return Err(invalid("Nomad transfer limits are invalid"));
    }
    Ok(NomadTransferLimits {
        maximum_manifest_paths: raw.maximum_manifest_paths,
        maximum_manifest_bytes: raw.maximum_manifest_bytes,
        maximum_input_nar_bytes: raw.maximum_input_nar_bytes,
        maximum_total_input_bytes: raw.maximum_total_input_bytes,
        maximum_output_nar_bytes: raw.maximum_output_nar_bytes,
        maximum_total_output_bytes: raw.maximum_total_output_bytes,
        maximum_frame_metadata_bytes: raw.maximum_frame_metadata_bytes,
        stream_buffer_bytes: raw.stream_buffer_bytes,
        maximum_live_log_chunk_bytes: raw.maximum_live_log_chunk_bytes,
        live_log_queue_bytes: raw.live_log_queue_bytes,
        transfer_idle_timeout: Duration::from_secs(raw.transfer_idle_timeout_seconds),
        setup_timeout: Duration::from_secs(raw.setup_timeout_seconds),
        output_collection_timeout: Duration::from_secs(raw.output_collection_timeout_seconds),
        maximum_connection_lifetime: Duration::from_secs(raw.maximum_connection_lifetime_seconds),
        authentication_lifetime: Duration::from_secs(raw.authentication_lifetime_seconds),
        clock_skew: Duration::from_secs(raw.clock_skew_seconds),
        nonce_retention: Duration::from_secs(raw.nonce_retention_seconds),
        reconnect_timeout: Duration::from_secs(raw.reconnect_timeout_seconds),
        maximum_diagnostic_bytes: raw.maximum_diagnostic_bytes,
    })
}

pub(super) fn validate_nomad_prestart(
    raw: RawNomadPrestartConfig,
) -> io::Result<NomadPrestartConfig> {
    if !valid_transfer_timeout(raw.timeout_seconds) {
        return Err(invalid("Nomad prestart timeout is invalid"));
    }
    Ok(NomadPrestartConfig {
        driver: validate_subject(raw.driver, "Nomad prestart task driver is invalid")?,
        driver_config: validate_driver_config(raw.driver_config)?,
        resources: validate_nomad_resources(raw.resources)?,
        timeout: Duration::from_secs(raw.timeout_seconds),
    })
}

pub(super) fn valid_transfer_bytes(value: u64) -> bool {
    value > 0 && value <= MAXIMUM_NOMAD_TRANSFER_BYTES
}

pub(super) fn valid_transfer_memory(value: usize) -> bool {
    value > 0 && value <= MAXIMUM_NOMAD_TRANSFER_MEMORY_BYTES
}

pub(super) fn valid_transfer_timeout(value: u64) -> bool {
    value > 0 && value <= MAXIMUM_NOMAD_TRANSFER_TIMEOUT_SECONDS
}

pub(super) fn validate_unique_backend_names(
    local: Option<&LocalBackendConfig>,
    static_ssh: &[StaticSshBackendConfig],
    nomad: &[NomadBackendConfig],
) -> io::Result<()> {
    let mut names = std::collections::HashSet::new();
    for name in local
        .map(|backend| backend.target().name())
        .into_iter()
        .chain(static_ssh.iter().map(|backend| backend.target().name()))
        .chain(nomad.iter().map(|backend| backend.target().name()))
    {
        if !names.insert(name) {
            return Err(invalid("backend name is ambiguous"));
        }
    }
    Ok(())
}

pub(super) fn validate_nomad_resources(raw: RawNomadResources) -> io::Result<NomadResources> {
    if raw.cpu_mhz == 0
        || raw.cpu_mhz > MAXIMUM_NOMAD_RESOURCE
        || raw.memory_mb == 0
        || raw.memory_mb > MAXIMUM_NOMAD_RESOURCE
        || raw.disk_mb == 0
        || raw.disk_mb > MAXIMUM_NOMAD_RESOURCE
    {
        return Err(invalid("Nomad resources are invalid"));
    }
    Ok(NomadResources {
        cpu_mhz: raw.cpu_mhz,
        memory_mb: raw.memory_mb,
        disk_mb: raw.disk_mb,
    })
}

pub(super) fn validate_driver_config(
    raw: toml::Table,
) -> io::Result<serde_json::Map<String, serde_json::Value>> {
    if raw.is_empty() || raw.len() > MAXIMUM_NOMAD_DRIVER_CONFIG_ENTRIES {
        return Err(invalid("Nomad driver configuration is invalid"));
    }
    let value = toml_to_json(toml::Value::Table(raw), 0)?;
    if serde_json::to_vec(&value)
        .map_err(|_| invalid("Nomad driver configuration is invalid"))?
        .len()
        > MAXIMUM_NOMAD_DRIVER_CONFIG_BYTES
    {
        return Err(invalid("Nomad driver configuration exceeds limit"));
    }
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Nomad driver configuration is invalid"))
}

pub(super) fn toml_to_json(value: toml::Value, depth: usize) -> io::Result<serde_json::Value> {
    if depth > MAXIMUM_NOMAD_DRIVER_CONFIG_DEPTH {
        return Err(invalid("Nomad driver configuration is invalid"));
    }
    match value {
        toml::Value::String(value) => Ok(value.into()),
        toml::Value::Integer(value) => Ok(value.into()),
        toml::Value::Float(value) if value.is_finite() => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| invalid("Nomad driver configuration is invalid")),
        toml::Value::Boolean(value) => Ok(value.into()),
        toml::Value::Array(values) => values
            .into_iter()
            .map(|value| toml_to_json(value, depth + 1))
            .collect::<io::Result<Vec<_>>>()
            .map(serde_json::Value::Array),
        toml::Value::Table(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, toml_to_json(value, depth + 1)?)))
            .collect::<io::Result<serde_json::Map<_, _>>>()
            .map(serde_json::Value::Object),
        toml::Value::Datetime(_) | toml::Value::Float(_) => {
            Err(invalid("Nomad driver configuration is invalid"))
        }
    }
}

pub(super) fn valid_nomad_endpoint(value: &str) -> bool {
    valid_endpoint(value, &["http://", "https://"])
}

pub(super) fn valid_nomad_transfer_endpoint(value: &str) -> bool {
    valid_endpoint(value, &["ws://", "wss://"])
}
