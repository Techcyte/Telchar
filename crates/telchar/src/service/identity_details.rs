//! Bounded public authentication metadata supplied by the SSH ingress.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityDetails {
    pub auth_method: String,
    pub key_fingerprint: Option<String>,
    pub key_type: Option<String>,
    pub certificate_ca_fingerprint: Option<String>,
    pub certificate_key_id: Option<String>,
    pub certificate_principals: Option<Vec<String>>,
    pub certificate_serial: Option<u64>,
    pub certificate_valid_from: Option<String>,
    pub certificate_valid_to: Option<String>,
    pub certificate_user_key_fingerprint: Option<String>,
    pub source_address: Option<IpAddr>,
    pub source_port: Option<u16>,
}

impl IdentityDetails {
    pub fn validate(&self) -> std::io::Result<()> {
        let strings = [
            self.key_fingerprint.as_deref(),
            self.key_type.as_deref(),
            self.certificate_ca_fingerprint.as_deref(),
            self.certificate_key_id.as_deref(),
            self.certificate_valid_from.as_deref(),
            self.certificate_valid_to.as_deref(),
            self.certificate_user_key_fingerprint.as_deref(),
        ];
        if !matches!(self.auth_method.as_str(), "publickey" | "certificate")
            || strings.into_iter().flatten().any(invalid_component)
            || self
                .certificate_principals
                .as_ref()
                .is_some_and(|principals| {
                    principals.len() > 64
                        || principals
                            .iter()
                            .any(|principal| invalid_component(principal))
                })
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "authentication audit metadata is invalid",
            ));
        }
        Ok(())
    }
}

fn invalid_component(value: &str) -> bool {
    value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
}
