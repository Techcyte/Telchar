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

pub(super) fn validate_ssh_backends(
    raw: Vec<RawSshConfig>,
) -> io::Result<(Vec<StaticSshBackendConfig>, Vec<StaticSshConsulConfig>)> {
    let mut static_backends = Vec::new();
    let mut consul_sources = Vec::new();
    for group in raw {
        for (pool_name, pool) in group.backends {
            let name = validate_subject(pool_name, "SSH backend name is invalid")?;
            let system = pool
                .system
                .clone()
                .or_else(|| group.system.clone())
                .unwrap_or_else(|| "x86_64-linux".to_owned());
            let features = pool
                .supported_features
                .clone()
                .or_else(|| group.supported_features.clone())
                .unwrap_or_default();
            let capacity = pool
                .maximum_concurrent_builds
                .or(group.maximum_concurrent_builds)
                .unwrap_or(1);
            let ssh_user = pool
                .ssh_user
                .clone()
                .or_else(|| group.ssh_user.clone())
                .unwrap_or_else(|| "telchar".to_owned());
            let identity_file = pool
                .identity_file
                .clone()
                .or_else(|| group.identity_file.clone())
                .ok_or_else(|| invalid("SSH identity file is required"))?;
            let known_hosts_file = pool
                .known_hosts_file
                .clone()
                .or_else(|| group.known_hosts_file.clone())
                .ok_or_else(|| invalid("SSH known-hosts file is required"))?;
            let ssh_program = pool
                .ssh_program
                .clone()
                .or_else(|| group.ssh_program.clone())
                .unwrap_or_else(|| {
                    PathBuf::from(PACKAGED_SSH_PROGRAM.unwrap_or(SYSTEM_SSH_PROGRAM))
                });
            match pool.source.as_str() {
                "static" => {
                    if pool.hosts.is_empty() || pool.endpoint.is_some() || pool.service.is_some() {
                        return Err(invalid("static SSH backend inventory is invalid"));
                    }
                    for (host_name, host) in pool.hosts {
                        let backend_name = format!("{name}.{host_name}");
                        let host_system = host.system.unwrap_or_else(|| system.clone());
                        let host_features =
                            host.supported_features.unwrap_or_else(|| features.clone());
                        let host_capacity = host.maximum_concurrent_builds.unwrap_or(capacity);
                        let host_user = host.ssh_user.unwrap_or_else(|| ssh_user.clone());
                        let host_identity =
                            host.identity_file.unwrap_or_else(|| identity_file.clone());
                        let host_known_hosts = host
                            .known_hosts_file
                            .unwrap_or_else(|| known_hosts_file.clone());
                        let host_program = host.ssh_program.unwrap_or_else(|| ssh_program.clone());
                        let ready = host
                            .ready_check_interval_seconds
                            .or(pool.ready_check_interval_seconds)
                            .or(group.ready_check_interval_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_READY_CHECK_INTERVAL_SECONDS);
                        let unavailable = host
                            .unavailable_check_interval_seconds
                            .or(pool.unavailable_check_interval_seconds)
                            .or(group.unavailable_check_interval_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_UNAVAILABLE_CHECK_INTERVAL_SECONDS);
                        let timeout = host
                            .check_timeout_seconds
                            .or(pool.check_timeout_seconds)
                            .or(group.check_timeout_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_CHECK_TIMEOUT_SECONDS);
                        validate_ssh_leaf(
                            host_capacity,
                            ready,
                            unavailable,
                            timeout,
                            &host_identity,
                            &host_known_hosts,
                            &host_program,
                        )?;
                        let destination = format!("{host_user}@{}", host.address);
                        if !valid_ssh_destination(&destination) || host.port == Some(0) {
                            return Err(invalid("static SSH destination is invalid"));
                        }
                        static_backends.push(StaticSshBackendConfig {
                            target: BackendTarget::new(
                                &backend_name,
                                BackendKind::StaticSsh,
                                &host_system,
                                &host_features,
                            )?,
                            maximum_concurrent_builds: host_capacity,
                            ready_check_interval: Duration::from_secs(ready),
                            unavailable_check_interval: Duration::from_secs(unavailable),
                            check_timeout: Duration::from_secs(timeout),
                            destination,
                            port: host.port.unwrap_or(22),
                            identity_file: host_identity,
                            known_hosts_file: host_known_hosts,
                            ssh_program: host_program,
                        });
                    }
                }
                "consul" => {
                    if !pool.hosts.is_empty() {
                        return Err(invalid("Consul SSH backend cannot contain static hosts"));
                    }
                    let endpoint = pool
                        .endpoint
                        .ok_or_else(|| invalid("Consul SSH endpoint is required"))?;
                    let service = pool
                        .service
                        .ok_or_else(|| invalid("Consul SSH service is required"))?;
                    let required_tags = pool.required_tags.unwrap_or_default();
                    let refresh = pool.refresh_interval_seconds.unwrap_or(15);
                    let request_timeout = pool.request_timeout_seconds.unwrap_or(5);
                    validate_ssh_leaf(
                        capacity,
                        pool.ready_check_interval_seconds
                            .or(group.ready_check_interval_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_READY_CHECK_INTERVAL_SECONDS),
                        pool.unavailable_check_interval_seconds
                            .or(group.unavailable_check_interval_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_UNAVAILABLE_CHECK_INTERVAL_SECONDS),
                        pool.check_timeout_seconds
                            .or(group.check_timeout_seconds)
                            .unwrap_or(DEFAULT_STATIC_SSH_CHECK_TIMEOUT_SECONDS),
                        &identity_file,
                        &known_hosts_file,
                        &ssh_program,
                    )?;
                    if !valid_endpoint(&endpoint, &["http://", "https://"])
                        || required_tags.len() > MAXIMUM_STATIC_SSH_CONSUL_TAGS
                        || refresh == 0
                        || refresh > MAXIMUM_STATIC_SSH_CONSUL_REFRESH_SECONDS
                        || request_timeout == 0
                        || request_timeout > MAXIMUM_STATIC_SSH_CONSUL_REQUEST_TIMEOUT_SECONDS
                        || request_timeout > refresh
                    {
                        return Err(invalid("Consul SSH backend is invalid"));
                    }
                    consul_sources.push(StaticSshConsulConfig {
                        name,
                        system,
                        supported_features: features,
                        maximum_concurrent_builds_per_instance: capacity,
                        endpoint,
                        service: validate_subject(service, "Consul SSH service is invalid")?,
                        datacenter: pool.datacenter,
                        required_tags,
                        passing_only: pool.passing_only.unwrap_or(true),
                        refresh_interval: Duration::from_secs(refresh),
                        request_timeout: Duration::from_secs(request_timeout),
                        token_file: pool.token_file,
                        ca_certificate_file: pool.ca_certificate_file,
                        ssh_user,
                        identity_file,
                        known_hosts_file,
                        ssh_program,
                    });
                }
                _ => return Err(invalid("SSH backend source is invalid")),
            }
        }
    }
    if static_backends.len() > MAXIMUM_STATIC_SSH_BACKENDS
        || consul_sources.len() > MAXIMUM_STATIC_SSH_CONSUL_SOURCES
    {
        return Err(invalid("SSH backend count exceeds limit"));
    }
    Ok((static_backends, consul_sources))
}

fn validate_ssh_leaf(
    capacity: usize,
    ready: u64,
    unavailable: u64,
    timeout: u64,
    identity_file: &Path,
    known_hosts_file: &Path,
    ssh_program: &Path,
) -> io::Result<()> {
    validate_backend_capacity(capacity)?;
    if ready == 0
        || ready > MAXIMUM_STATIC_SSH_CHECK_INTERVAL_SECONDS
        || unavailable == 0
        || unavailable > MAXIMUM_STATIC_SSH_CHECK_INTERVAL_SECONDS
        || timeout == 0
        || timeout > MAXIMUM_STATIC_SSH_CHECK_TIMEOUT_SECONDS
    {
        return Err(invalid("SSH health timing bounds are invalid"));
    }
    validate_identity_file(identity_file)?;
    validate_known_hosts_file(known_hosts_file)?;
    validate_executable_file(ssh_program, "SSH program is invalid")
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
        if backend.max_retries > MAXIMUM_NOMAD_RETRIES {
            return Err(invalid("Nomad retry count exceeds limit"));
        }
        if !valid_nomad_endpoint(&backend.endpoint) {
            return Err(invalid("Nomad endpoint is invalid"));
        }
        let namespace = validate_subject(backend.namespace, "Nomad namespace is invalid")?;
        let node_pool = validate_subject(
            backend.node_pool.unwrap_or_else(|| "default".to_owned()),
            "Nomad node pool is invalid",
        )?;
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
        let priority = validate_nomad_priority(backend.priority)?;
        let constraints = validate_nomad_constraints(backend.constraints)?;
        let resource_profiles = validate_nomad_resource_profiles(
            backend.resource_profiles,
            &backend.supported_features,
            constraints.len(),
        )?;
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
        let callback_connect = backend
            .callback_connect
            .map(|connect| -> io::Result<NomadCallbackConnect> {
                if connect.local_bind_port == 0 {
                    return Err(invalid("Nomad callback Connect port is invalid"));
                }
                Ok(NomadCallbackConnect {
                    source_service: validate_subject(
                        connect.source_service,
                        "Nomad callback source service is invalid",
                    )?,
                    destination_service: validate_subject(
                        connect.destination_service,
                        "Nomad callback destination service is invalid",
                    )?,
                    local_bind_port: connect.local_bind_port,
                })
            })
            .transpose()?;
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
            max_retries: backend.max_retries,
            endpoint: backend.endpoint,
            namespace,
            node_pool,
            token_file,
            ca_certificate_file,
            client_certificate_file,
            client_key_file,
            driver,
            driver_config,
            resources,
            priority,
            resource_profiles,
            job_name_scope,
            poll_interval: Duration::from_secs(backend.poll_interval_seconds),
            runtime_limit: Duration::from_secs(backend.runtime_limit_seconds),
            constraints,
            transfer_endpoint,
            callback_connect,
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
            verify_issuer,
            jwks_url,
            audience,
            ca_certificate_file,
        } => {
            if issuer
                .as_deref()
                .is_some_and(|value| !valid_nomad_endpoint(value))
                || !valid_nomad_endpoint(&jwks_url)
                || (verify_issuer && issuer.is_none())
            {
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
                verify_issuer,
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

pub(super) fn validate_nomad_constraints(
    raw: Vec<RawNomadConstraint>,
) -> io::Result<Vec<NomadConstraint>> {
    if raw.len() > MAXIMUM_NOMAD_CONSTRAINTS {
        return Err(invalid("Nomad constraint count exceeds limit"));
    }
    raw.into_iter()
        .map(|constraint| {
            if constraint.attribute.is_empty()
                || constraint.attribute.len() > MAXIMUM_NOMAD_CONSTRAINT_FIELD_BYTES
                || constraint.operator.is_empty()
                || constraint.operator.len() > MAXIMUM_NOMAD_CONSTRAINT_FIELD_BYTES
                || constraint.value.is_empty()
                || constraint.value.len() > MAXIMUM_NOMAD_CONSTRAINT_FIELD_BYTES
            {
                return Err(invalid("Nomad constraint is invalid"));
            }
            Ok(NomadConstraint {
                attribute: constraint.attribute,
                operator: constraint.operator,
                value: constraint.value,
            })
        })
        .collect()
}

pub(super) fn validate_nomad_priority(raw: Option<RawNomadPriority>) -> io::Result<NomadPriority> {
    let raw = raw.unwrap_or(RawNomadPriority {
        minimum: DEFAULT_NOMAD_PRIORITY,
        default: DEFAULT_NOMAD_PRIORITY,
        maximum: DEFAULT_NOMAD_PRIORITY,
    });
    validate_nomad_priority_values(raw.minimum, raw.default, raw.maximum)
}

fn validate_nomad_priority_values(
    minimum: u8,
    default: u8,
    maximum: u8,
) -> io::Result<NomadPriority> {
    if minimum < MINIMUM_NOMAD_PRIORITY
        || maximum > MAXIMUM_NOMAD_PRIORITY
        || minimum > default
        || default > maximum
    {
        return Err(invalid("Nomad priority is invalid"));
    }
    Ok(NomadPriority {
        minimum,
        default,
        maximum,
    })
}

pub(super) fn validate_nomad_resource_profiles(
    raw: Vec<RawNomadResourceProfile>,
    supported_features: &[String],
    base_constraint_count: usize,
) -> io::Result<Vec<NomadResourceProfile>> {
    if raw.len() > MAXIMUM_NOMAD_RESOURCE_PROFILES {
        return Err(invalid("Nomad resource profile count exceeds limit"));
    }
    let mut profiles = Vec::with_capacity(raw.len());
    for profile in raw {
        if profile.name == "default"
            || profiles.iter().any(|existing: &NomadResourceProfile| {
                existing.name() == profile.name
                    || existing.required_feature() == profile.required_feature
            })
            || !supported_features
                .iter()
                .any(|feature| feature == &profile.required_feature)
        {
            return Err(invalid("Nomad resource profile is invalid"));
        }
        let name = validate_profile_component(profile.name)?;
        let required_feature = validate_profile_component(profile.required_feature)?;
        let resources = validate_nomad_resources(RawNomadResources {
            cpu_mhz: profile.cpu_mhz,
            memory_mb: profile.memory_mb,
            disk_mb: profile.disk_mb,
        })?;
        let priority = validate_nomad_priority_values(
            profile.priority_minimum,
            profile.priority_default,
            profile.priority_maximum,
        )?;
        if base_constraint_count.saturating_add(profile.constraints.len())
            > MAXIMUM_NOMAD_CONSTRAINTS
        {
            return Err(invalid("Nomad constraint count exceeds limit"));
        }
        let constraints = validate_nomad_constraints(profile.constraints)?;
        profiles.push(NomadResourceProfile {
            name,
            required_feature,
            resources,
            priority,
            constraints,
        });
    }
    Ok(profiles)
}

fn validate_profile_component(value: String) -> io::Result<String> {
    if value.is_empty()
        || value.len() > MAXIMUM_SUBJECT_BYTES
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(invalid("Nomad resource profile is invalid"));
    }
    Ok(value)
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
