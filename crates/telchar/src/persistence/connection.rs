use std::path::PathBuf;

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use postgres::{Client, Config, NoTls};
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;
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
    pool: DatabasePool,
}

#[derive(Clone)]
enum DatabasePool {
    Plain(Pool<PostgresConnectionManager<NoTls>>),
    Tls(Pool<PostgresConnectionManager<MakeRustlsConnect>>),
}

pub enum DatabaseConnection {
    Plain(PooledConnection<PostgresConnectionManager<NoTls>>),
    Tls(PooledConnection<PostgresConnectionManager<MakeRustlsConnect>>),
}

impl Database {
    pub fn connect(database_url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (config, security) = connection_config(database_url)?;
        let pool = match security {
            ConnectionSecurity::Plain => {
                DatabasePool::Plain(build_pool(PostgresConnectionManager::new(config, NoTls))?)
            }
            ConnectionSecurity::Tls { root_certificate } => DatabasePool::Tls(build_pool(
                PostgresConnectionManager::new(config, tls_connector(root_certificate)?),
            )?),
        };
        Ok(Self { pool })
    }

    pub fn connection(
        &self,
    ) -> Result<DatabaseConnection, Box<dyn std::error::Error + Send + Sync>> {
        match &self.pool {
            DatabasePool::Plain(pool) => Ok(DatabaseConnection::Plain(pool.get()?)),
            DatabasePool::Tls(pool) => Ok(DatabaseConnection::Tls(pool.get()?)),
        }
    }
}

pub trait DatabaseSource {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>>;
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
}

impl DatabaseSource for String {
    fn database(&self) -> Result<Database, Box<dyn std::error::Error + Send + Sync>> {
        self.as_str().database()
    }
}

pub(crate) fn connect(
    database: &(impl DatabaseSource + ?Sized),
) -> Result<DatabaseConnection, Box<dyn std::error::Error + Send + Sync>> {
    database.database()?.connection()
}

impl Deref for DatabaseConnection {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Plain(connection) => connection,
            Self::Tls(connection) => connection,
        }
    }
}

impl DerefMut for DatabaseConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Plain(connection) => connection,
            Self::Tls(connection) => connection,
        }
    }
}

fn build_pool<T>(
    manager: PostgresConnectionManager<T>,
) -> Result<Pool<PostgresConnectionManager<T>>, r2d2::Error>
where
    T: postgres::tls::MakeTlsConnect<postgres::Socket> + Clone + Send + Sync + 'static,
    T::Stream: Send + Sync,
    T::TlsConnect: Send,
    <T::TlsConnect as postgres::tls::TlsConnect<postgres::Socket>>::Future: Send,
{
    Pool::builder()
        .max_size(16)
        .min_idle(Some(1))
        .connection_timeout(Duration::from_secs(5))
        .build(manager)
}

fn tls_connector(
    root_certificate: Option<PathBuf>,
) -> Result<MakeRustlsConnect, Box<dyn std::error::Error + Send + Sync>> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for certificate in native.certs {
        roots.add(certificate)?;
    }
    if let Some(path) = root_certificate {
        let certificate = std::fs::read(path)?;
        let mut certificate = certificate.as_slice();
        for certificate in rustls_pemfile::certs(&mut certificate) {
            roots.add(certificate?)?;
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

fn connection_config(
    database_url: &str,
) -> Result<(Config, ConnectionSecurity), Box<dyn std::error::Error + Send + Sync>> {
    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        return Ok((database_url.parse::<Config>()?, ConnectionSecurity::Plain));
    }

    let mut url = Url::parse(database_url)?;
    let mut tls_requested = false;
    let mut root_certificate = None;
    let mut parameters = Vec::new();
    let query_parameters = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    for (key, value) in query_parameters {
        match key.as_str() {
            "sslrootcert" => root_certificate = Some(PathBuf::from(value)),
            "sslmode" if matches!(value.as_str(), "verify-ca" | "verify-full") => {
                tls_requested = true;
                parameters.push((key, "require".to_owned()));
            }
            "sslmode" if value == "require" => {
                tls_requested = true;
                parameters.push((key, value));
            }
            _ => parameters.push((key, value)),
        }
    }
    url.set_query(None);
    if !parameters.is_empty() {
        url.query_pairs_mut().extend_pairs(parameters);
    }
    let normalized_database_url = url.as_str().replace('+', "%20");
    let config = normalized_database_url.parse::<Config>()?;
    let security = if tls_requested {
        ConnectionSecurity::Tls { root_certificate }
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
        assert_eq!(
            connection_security(
                "postgresql://telchar@db.example.com/telchar?sslmode=verify-full&sslrootcert=%2Frun%2Fsecrets%2Frds-ca.pem",
            )
            .expect("TLS URL parses"),
            ConnectionSecurity::Tls {
                root_certificate: Some("/run/secrets/rds-ca.pem".into()),
            }
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
    fn url_options_preserve_spaces() {
        let (config, security) = connection_config(
            "postgresql://telchar@localhost/telchar?options=-c%20telchar.owner_kind%3Dgateway",
        )
        .expect("URL options parse");

        assert_eq!(security, ConnectionSecurity::Plain);
        assert_eq!(config.get_options(), Some("-c telchar.owner_kind=gateway"));
    }
}
