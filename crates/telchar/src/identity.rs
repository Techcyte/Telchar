use std::fmt;
use std::net::IpAddr;

const MAX_IDENTITY_COMPONENT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyIdentity {
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateIdentity {
    pub ca_fingerprint: String,
    pub key_id: String,
    pub principals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    pub credential_id: String,
    pub audit_subject: String,
    pub quota_subject: String,
    pub certificate: Option<CertificateIdentity>,
    pub source_address: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityInput {
    PublicKey {
        fingerprint: String,
        audit_subject: Option<String>,
        quota_subject: Option<String>,
        source_address: Option<IpAddr>,
    },
    Certificate {
        ca_fingerprint: String,
        key_id: String,
        principals: Vec<String>,
        audit_subject: Option<String>,
        quota_subject: Option<String>,
        source_address: Option<IpAddr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    EmptyComponent(&'static str),
    OversizedComponent(&'static str),
    EmptyPrincipal,
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyComponent(name) => write!(formatter, "{name} is empty"),
            Self::OversizedComponent(name) => write!(formatter, "{name} is oversized"),
            Self::EmptyPrincipal => formatter.write_str("certificate principal is empty"),
        }
    }
}

impl std::error::Error for NormalizeError {}

pub fn normalize_requester(input: IdentityInput) -> Result<Requester, NormalizeError> {
    match input {
        IdentityInput::PublicKey {
            fingerprint,
            audit_subject,
            quota_subject,
            source_address,
        } => {
            validate_component("fingerprint", &fingerprint)?;
            let credential_id = format!("ssh-pubkey:{fingerprint}");
            let audit_subject = choose_subject("audit subject", audit_subject, &fingerprint)?;
            let quota_subject = choose_subject("quota subject", quota_subject, &credential_id)?;
            Ok(Requester {
                credential_id,
                audit_subject,
                quota_subject,
                certificate: None,
                source_address,
            })
        }
        IdentityInput::Certificate {
            ca_fingerprint,
            key_id,
            principals,
            audit_subject,
            quota_subject,
            source_address,
        } => {
            validate_component("CA fingerprint", &ca_fingerprint)?;
            validate_component("certificate key ID", &key_id)?;
            if principals.is_empty() {
                return Err(NormalizeError::EmptyPrincipal);
            }
            for principal in &principals {
                validate_component("certificate principal", principal)?;
            }
            let credential_id = certificate_credential_id(&ca_fingerprint, &key_id);
            let audit_subject = choose_subject("audit subject", audit_subject, &principals[0])?;
            let quota_subject = choose_subject("quota subject", quota_subject, &credential_id)?;
            Ok(Requester {
                credential_id,
                audit_subject,
                quota_subject,
                certificate: Some(CertificateIdentity {
                    ca_fingerprint,
                    key_id,
                    principals,
                }),
                source_address,
            })
        }
    }
}

fn certificate_credential_id(ca_fingerprint: &str, key_id: &str) -> String {
    format!(
        "ssh-cert:{}:{ca_fingerprint}:{}:{key_id}",
        ca_fingerprint.len(),
        key_id.len()
    )
}

fn choose_subject(
    name: &'static str,
    configured: Option<String>,
    fallback: &str,
) -> Result<String, NormalizeError> {
    let subject = configured.unwrap_or_else(|| fallback.to_owned());
    validate_component(name, &subject)?;
    Ok(subject)
}

fn validate_component(name: &'static str, value: &str) -> Result<(), NormalizeError> {
    if value.is_empty() {
        return Err(NormalizeError::EmptyComponent(name));
    }
    if value.len() > MAX_IDENTITY_COMPONENT {
        return Err(NormalizeError::OversizedComponent(name));
    }
    Ok(())
}
