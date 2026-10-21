use std::path::PathBuf;

use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

use r2d2::{Pool, PooledConnection};
use tokio_postgres::Config;

mod io;
pub use io::{DatabaseClient, DatabaseError, DatabaseTransaction};
use io::{Manager, Security};
use rustls::{ClientConfig, RootCertStore};
use tokio_postgres_rustls::MakeRustlsConnect;
use url::Url;

#[derive(Debug, Eq, PartialEq)]
enum ConnectionSecurity {
    Plain,
    Tls { root_certificate: Option<PathBuf> },
}

#[derive(Clone)]
pub struct Database {
    pool: Pool<Manager>,
}

pub struct DatabaseConnection(PooledConnection<Manager>);

impl Database {
    pub fn connect(database_url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (config, security) = connection_config(database_url)?;
        let security = match security {
            ConnectionSecurity::Plain => Security::Plain,
            ConnectionSecurity::Tls { root_certificate } => {
                Security::Tls(tls_connector(root_certificate)?)
            }
        };
        Ok(Self {
            pool: build_pool(Manager { config, security })?,
        })
    }

    pub fn connection(
        &self,
    ) -> Result<DatabaseConnection, Box<dyn std::error::Error + Send + Sync>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connection = self
            .pool
            .get_timeout(deadline.saturating_duration_since(Instant::now()))?;
        connection.validate(deadline)?;
        Ok(DatabaseConnection(connection))
    }
}

pub trait DatabaseSource {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>>;

    fn is_configured(&self) -> bool {
        true
    }
}

impl DatabaseSource for Database {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.clone())
    }
}

impl DatabaseSource for str {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>> {
        Database::connect(self)
    }

    fn is_configured(&self) -> bool {
        !self.trim().is_empty()
    }
}

impl DatabaseSource for String {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>> {
        self.as_str().database()
    }

    fn is_configured(&self) -> bool {
        self.as_str().is_configured()
    }
}

pub(crate) fn connect(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<DatabaseConnection, Box<dyn std::error::Error + Send + Sync>> {
    database.database()?.connection()
}

impl Deref for DatabaseConnection {
    type Target = DatabaseClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DatabaseConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn build_pool(manager: Manager) -> Result<Pool<Manager>, r2d2::Error> {
    let maintenance = scheduled_thread_pool::ScheduledThreadPool::builder()
        .num_threads(3)
        .thread_name_pattern("r2d2-worker-{}")
        .on_drop_behavior(scheduled_thread_pool::OnPoolDropBehavior::DiscardPendingScheduled)
        .build();
    Ok(Pool::builder()
        .thread_pool(std::sync::Arc::new(maintenance))
        .max_size(16)
        // Validation runs after checkout under the caller's remaining absolute budget.
        .test_on_check_out(false)
        .connection_timeout(Duration::from_secs(5))
        .build_unchecked(manager))
}

fn tls_connector(
    root_certificate: Option<PathBuf>,
) -> Result<MakeRustlsConnect, Box<dyn std::error::Error + Send + Sync>> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = root_certificate {
        let certificate = std::fs::read(path)?;
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        for certificate in CertificateDer::pem_slice_iter(&certificate) {
            roots.add(certificate?)?;
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        for certificate in native.certs {
            roots.add(certificate)?;
        }
    }
    let tls = ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(tls))
}

pub(crate) fn validate(database_url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    connection_config(database_url).map(|_| ())
}

pub fn validate_verified_connection(
    database_url: &str,
    expected_root_certificate: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = connection_settings(database_url)?;
    if settings.ssl_mode.as_deref() != Some("verify-full")
        || settings.root_certificate.as_deref() != Some(expected_root_certificate)
    {
        return Err("database URL must use effective sslmode=verify-full and the configured root certificate".into());
    }
    Ok(())
}

#[derive(Default)]
struct ConnectionSettings {
    ssl_mode: Option<String>,
    root_certificate: Option<PathBuf>,
}

fn connection_settings(
    database_url: &str,
) -> Result<ConnectionSettings, Box<dyn std::error::Error + Send + Sync>> {
    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        return Ok(ConnectionSettings::default());
    }
    let url = Url::parse(database_url)?;
    let mut settings = ConnectionSettings::default();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "sslmode" => settings.ssl_mode = Some(value.into_owned()),
            "sslrootcert" => settings.root_certificate = Some(PathBuf::from(value.into_owned())),
            _ => {}
        }
    }
    Ok(settings)
}

fn connection_config(
    database_url: &str,
) -> Result<(Config, ConnectionSecurity), Box<dyn std::error::Error + Send + Sync>> {
    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        return Ok((database_url.parse::<Config>()?, ConnectionSecurity::Plain));
    }

    let mut url = Url::parse(database_url)?;
    let settings = connection_settings(database_url)?;
    let tls_requested = matches!(
        settings.ssl_mode.as_deref(),
        Some("verify-ca" | "verify-full" | "require")
    );
    let mut parameters = Vec::new();
    let query_parameters = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    for (key, value) in query_parameters {
        match key.as_str() {
            "sslrootcert" | "sslmode" => {}
            _ => parameters.push((key, value)),
        }
    }
    if tls_requested {
        parameters.push(("sslmode".to_owned(), "require".to_owned()));
    }
    url.set_query(None);
    if !parameters.is_empty() {
        url.query_pairs_mut().extend_pairs(parameters);
    }
    let normalized_database_url = match url.as_str().split_once('?') {
        Some((base, query)) => format!("{base}?{}", query.replace('+', "%20")),
        None => url.as_str().to_owned(),
    };
    let config = normalized_database_url.parse::<Config>()?;
    let security = if tls_requested {
        ConnectionSecurity::Tls {
            root_certificate: settings.root_certificate,
        }
    } else {
        ConnectionSecurity::Plain
    };
    Ok((config, security))
}

#[cfg(test)]
fn connection_security(
    database_url: &str,
) -> Result<ConnectionSecurity, Box<dyn std::error::Error + Send + Sync>> {
    connection_config(database_url).map(|(_, security)| security)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_database_urls_select_unencrypted_connections() {
        assert_eq!(
            connection_security("postgresql://telchar@localhost/telchar?sslmode=disable")
                .expect("plain URL parses"),
            ConnectionSecurity::Plain
        );
    }

    #[test]
    fn verified_database_urls_select_tls_and_extract_the_root_certificate() {
        let database_url = "postgresql://telchar@db.example.com/telchar?sslmode=verify-full&sslrootcert=%2Frun%2Fsecrets%2Frds-ca.pem";
        assert_eq!(
            connection_security(database_url).expect("TLS URL parses"),
            ConnectionSecurity::Tls {
                root_certificate: Some("/run/secrets/rds-ca.pem".into()),
            }
        );
        validate_verified_connection(
            database_url,
            std::path::Path::new("/run/secrets/rds-ca.pem"),
        )
        .expect("verified connection is accepted");
    }

    #[test]
    fn decoy_verified_tls_parameters_do_not_override_effective_plain_mode() {
        let database_url = "postgresql://telchar@db.example.com/telchar?sslmode=disable&options=sslmode=verify-full&application_name=sslrootcert=/run/secrets/rds-ca.pem";
        assert_eq!(
            connection_security(database_url).expect("plain URL parses"),
            ConnectionSecurity::Plain
        );
        assert!(
            validate_verified_connection(
                database_url,
                std::path::Path::new("/run/secrets/rds-ca.pem")
            )
            .is_err()
        );
    }

    #[test]
    fn last_duplicate_tls_parameters_are_effective() {
        let database_url = "postgresql://telchar@db.example.com/telchar?sslmode=verify-full&sslrootcert=%2Fwrong.pem&sslmode=disable&sslrootcert=%2Frun%2Fsecrets%2Frds-ca.pem";
        assert_eq!(
            connection_security(database_url).expect("duplicate URL parses"),
            ConnectionSecurity::Plain
        );
        assert!(
            validate_verified_connection(
                database_url,
                std::path::Path::new("/run/secrets/rds-ca.pem")
            )
            .is_err()
        );
    }

    #[test]
    fn database_urls_without_tls_configuration_select_unencrypted_connections() {
        assert_eq!(
            connection_security("postgresql://telchar@db.example.com/telchar")
                .expect("default URL parses"),
            ConnectionSecurity::Plain
        );
    }

    #[test]
    fn keyword_database_configuration_preserves_unencrypted_connections() {
        assert_eq!(
            connection_security("host=/run/postgresql user=telchar dbname=telchar")
                .expect("keyword configuration parses"),
            ConnectionSecurity::Plain
        );
    }

    #[test]
    fn url_components_preserve_literal_plus_signs() {
        let (config, security) = connection_config(
            "postgresql://user+name:pass+word@localhost/database+name?application_name=client%2Bname",
        )
        .expect("URL with plus signs parses");

        assert_eq!(security, ConnectionSecurity::Plain);
        assert_eq!(config.get_user(), Some("user+name"));
        assert_eq!(config.get_password(), Some("pass+word".as_bytes()));
        assert_eq!(config.get_dbname(), Some("database+name"));
        assert_eq!(config.get_application_name(), Some("client+name"));
    }

    #[test]
    fn url_options_preserve_spaces() {
        let (config, security) = connection_config(
            "postgresql://telchar@localhost/telchar?options=-c%20telchar.owner_kind%3Dgateway",
        )
        .expect("URL options parse");

        assert_eq!(security, ConnectionSecurity::Plain);
        assert_eq!(config.get_options(), Some("-c telchar.owner_kind=gateway"));
    }
}
