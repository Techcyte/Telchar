use std::path::PathBuf;

use postgres::config::SslMode;
use postgres::{Client, Config, NoTls};
use rustls::{ClientConfig, RootCertStore};
use tokio_postgres_rustls::MakeRustlsConnect;
use url::Url;

#[derive(Debug, Eq, PartialEq)]
enum ConnectionSecurity {
    Plain,
    Tls { root_certificate: Option<PathBuf> },
}

pub(super) fn connect(
    database_url: &str,
) -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    let (config, security) = connection_config(database_url)?;
    match security {
        ConnectionSecurity::Plain => Ok(config.connect(NoTls)?),
        ConnectionSecurity::Tls { root_certificate } => {
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
            let tls = ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
            Ok(config.connect(MakeRustlsConnect::new(tls))?)
        }
    }
}

fn connection_config(
    database_url: &str,
) -> Result<(Config, ConnectionSecurity), Box<dyn std::error::Error + Send + Sync>> {
    let mut url = Url::parse(database_url)?;
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
                parameters.push((key, "require".to_owned()));
            }
            _ => parameters.push((key, value)),
        }
    }
    url.set_query(None);
    if !parameters.is_empty() {
        url.query_pairs_mut().extend_pairs(parameters);
    }
    let config = url.as_str().parse::<Config>()?;
    let security = if config.get_ssl_mode() == SslMode::Disable {
        ConnectionSecurity::Plain
    } else {
        ConnectionSecurity::Tls { root_certificate }
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
    fn default_database_urls_select_tls() {
        assert_eq!(
            connection_security("postgresql://telchar@db.example.com/telchar")
                .expect("default URL parses"),
            ConnectionSecurity::Tls {
                root_certificate: None,
            }
        );
    }
}
