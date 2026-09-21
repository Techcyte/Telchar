//! Maps EC2 membership into ordinary SSH execution targets.

use std::collections::BTreeSet;
use std::io;

use aws_sdk_ec2::types::{Instance, InstanceStateName};

use crate::service::config::{Ec2Address, StaticSshBackendConfig, StaticSshEc2Config};

pub fn map_instances(
    config: &StaticSshEc2Config,
    instances: &[Instance],
) -> io::Result<Vec<StaticSshBackendConfig>> {
    let mut members = Vec::new();
    let mut identities = BTreeSet::new();
    let mut addresses = BTreeSet::new();
    for instance in instances {
        if instance.state().and_then(|state| state.name()) != Some(&InstanceStateName::Running)
            || !config.tags().iter().all(|(key, value)| {
                instance
                    .tags()
                    .iter()
                    .any(|tag| tag.key() == Some(key) && tag.value() == Some(value))
            })
        {
            continue;
        }
        let address = match config.address() {
            Ec2Address::PrivateIp => instance.private_ip_address(),
            Ec2Address::PublicIp => instance.public_ip_address(),
        };
        let Some(address) = address else { continue };
        let address = address
            .parse::<std::net::IpAddr>()
            .map_err(|_| invalid("EC2 instance address is invalid"))?;
        let identity = instance
            .instance_id()
            .ok_or_else(|| invalid("EC2 instance identity is missing"))?;
        let suffix = identity
            .strip_prefix("i-")
            .ok_or_else(|| invalid("EC2 instance identity is invalid"))?;
        if !matches!(suffix.len(), 8 | 17) || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("EC2 instance identity is invalid"));
        }
        if !identities.insert(identity) || !addresses.insert(address) {
            return Err(invalid("EC2 membership is ambiguous"));
        }
        if members.len() >= 256 {
            return Err(invalid("EC2 membership exceeds limit"));
        }
        members.push(config.member(identity, address)?);
    }
    members.sort_by(|left, right| left.target().name().cmp(right.target().name()));
    Ok(members)
}

pub struct Ec2SshDiscovery {
    config: StaticSshEc2Config,
    client: aws_sdk_ec2::Client,
    inventory: Vec<StaticSshBackendConfig>,
}

impl Ec2SshDiscovery {
    pub fn with_client(config: StaticSshEc2Config, client: aws_sdk_ec2::Client) -> Self {
        Self {
            config,
            client,
            inventory: Vec::new(),
        }
    }

    pub fn inventory(&self) -> &[StaticSshBackendConfig] {
        &self.inventory
    }

    pub async fn refresh(&mut self) -> io::Result<usize> {
        let members = tokio::time::timeout(self.config.request_timeout, self.fetch())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "EC2 discovery timed out"))??;
        let count = members.len();
        self.inventory = members;
        Ok(count)
    }

    async fn fetch(&self) -> io::Result<Vec<StaticSshBackendConfig>> {
        use aws_sdk_ec2::types::Filter;
        let mut filters = vec![
            Filter::builder()
                .name("instance-state-name")
                .values("running")
                .build(),
        ];
        for (key, value) in self.config.tags() {
            filters.push(
                Filter::builder()
                    .name(format!("tag:{key}"))
                    .values(value)
                    .build(),
            );
        }
        let mut token = None;
        let mut tokens = BTreeSet::new();
        let mut instances = Vec::new();
        for _ in 0..64 {
            let page = self
                .client
                .describe_instances()
                .set_filters(Some(filters.clone()))
                .max_results(100)
                .set_next_token(token)
                .send()
                .await
                .map_err(|_| io::Error::other("EC2 discovery request failed"))?;
            for reservation in page.reservations() {
                instances.extend_from_slice(reservation.instances());
                if instances.len() > 256 {
                    return Err(invalid("EC2 membership exceeds limit"));
                }
            }
            token = page
                .next_token()
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
            let Some(next) = &token else {
                return map_instances(&self.config, &instances);
            };
            if next.len() > 16384 || !tokens.insert(next.clone()) {
                return Err(invalid("EC2 pagination is invalid"));
            }
        }
        Err(invalid("EC2 pagination exceeds limit"))
    }
}

pub struct Ec2SshDiscoveryService {
    stop: Option<std::sync::mpsc::Sender<()>>,
    updates: std::sync::Arc<std::sync::Mutex<Option<Vec<StaticSshBackendConfig>>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Ec2SshDiscoveryService {
    pub fn start(configs: Vec<StaticSshEc2Config>) -> io::Result<Self> {
        if configs.is_empty() {
            return Err(invalid("EC2 discovery requires a pool"));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut discoveries = Vec::new();
        for config in configs {
            let credentials = match config.credentials.clone() {
                Some(files) => aws_credential_types::provider::SharedCredentialsProvider::new(
                    CredentialFiles::new(files),
                ),
                None => aws_credential_types::provider::SharedCredentialsProvider::new(
                    aws_config::imds::credentials::ImdsCredentialsProvider::builder().build(),
                ),
            };
            let client = {
                let _entered = runtime.enter();
                let mut builder = aws_sdk_ec2::config::Builder::new()
                    .behavior_version_latest()
                    .region(aws_sdk_ec2::config::Region::new(config.region.clone()))
                    .credentials_provider(credentials)
                    .retry_config(aws_sdk_ec2::config::retry::RetryConfig::disabled())
                    .timeout_config(
                        aws_sdk_ec2::config::timeout::TimeoutConfig::builder()
                            .operation_timeout(config.request_timeout)
                            .build(),
                    );
                if config.credentials.is_some() {
                    builder =
                        builder.identity_cache(aws_sdk_ec2::config::IdentityCache::no_cache());
                }
                aws_sdk_ec2::Client::from_conf(builder.build())
            };
            discoveries.push((
                Ec2SshDiscovery::with_client(config, client),
                std::time::Instant::now(),
            ));
        }
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let updates = std::sync::Arc::new(std::sync::Mutex::new(None));
        let publish = std::sync::Arc::clone(&updates);
        let worker = std::thread::Builder::new()
            .name("ec2-discovery".to_owned())
            .spawn(move || {
                loop {
                    let mut changed = false;
                    for (discovery, next) in &mut discoveries {
                        if !matches!(
                            stop_rx.try_recv(),
                            Err(std::sync::mpsc::TryRecvError::Empty)
                        ) {
                            return;
                        }
                        if std::time::Instant::now() >= *next {
                            match runtime.block_on(discovery.refresh()) {
                                Ok(_) => changed = true,
                                Err(_) => tracing::warn!(
                                    event = "ec2.discovery.failed",
                                    "EC2 discovery refresh failed; retaining inventory"
                                ),
                            }
                            *next = std::time::Instant::now() + discovery.config.refresh_interval;
                        }
                    }
                    if changed {
                        let inventory = discoveries
                            .iter()
                            .flat_map(|(discovery, _)| discovery.inventory().iter().cloned())
                            .collect();
                        *publish
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(inventory);
                    }
                    let wait = discoveries
                        .iter()
                        .map(|(_, next)| next.saturating_duration_since(std::time::Instant::now()))
                        .min()
                        .unwrap_or_default();
                    match stop_rx.recv_timeout(wait) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop_tx),
            updates,
            worker: Some(worker),
        })
    }

    pub fn check(&mut self) -> io::Result<Option<Vec<StaticSshBackendConfig>>> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err(io::Error::other("EC2 discovery service stopped"));
        }
        Ok(self
            .updates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take())
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("EC2 discovery service failed"))?;
        }
        Ok(())
    }
}

impl Drop for Ec2SshDiscoveryService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug)]
pub struct CredentialFiles(crate::service::config::Ec2CredentialsConfig);

impl CredentialFiles {
    pub fn new(config: crate::service::config::Ec2CredentialsConfig) -> Self {
        Self(config)
    }
}

impl aws_credential_types::provider::ProvideCredentials for CredentialFiles {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        aws_credential_types::provider::future::ProvideCredentials::new(async move {
            let read = |path: &std::path::Path| {
                read_credential(path).map_err(|_| {
                    aws_credential_types::provider::error::CredentialsError::provider_error(
                        "EC2 credential file is unavailable or invalid",
                    )
                })
            };
            Ok(aws_credential_types::Credentials::new(
                read(&self.0.access_key_id_file)?,
                read(&self.0.secret_access_key_file)?,
                self.0.session_token_file.as_deref().map(read).transpose()?,
                None,
                "telchar-credential-files",
            ))
        })
    }
}

fn read_credential(path: &std::path::Path) -> io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.len() > 16384 {
        return Err(invalid("EC2 credential file is invalid"));
    }
    let mut value = String::new();
    file.take(16385).read_to_string(&mut value)?;
    let value = value.trim();
    if value.is_empty()
        || value.len() > 16384
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(invalid("EC2 credential file is invalid"));
    }
    Ok(value.to_owned())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
