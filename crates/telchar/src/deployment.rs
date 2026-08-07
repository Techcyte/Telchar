use std::collections::BTreeSet;
use std::io;

const MAXIMUM_SUPPORTED_FEATURES: usize = 64;
const MAXIMUM_SYSTEM_BYTES: usize = 64;
const MAXIMUM_FEATURE_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentConfig {
    system: String,
    supported_features: Vec<String>,
}

impl DeploymentConfig {
    pub fn parse(system: &str, supported_features: &str) -> io::Result<Self> {
        if !valid_name(system, MAXIMUM_SYSTEM_BYTES) || system.contains(',') {
            return Err(invalid("deployment requires exactly one valid Nix system"));
        }

        let mut features = BTreeSet::new();
        if !supported_features.is_empty() {
            for feature in supported_features.split(',') {
                if features.len() >= MAXIMUM_SUPPORTED_FEATURES {
                    return Err(invalid("deployment supported feature count exceeds limit"));
                }
                if !valid_name(feature, MAXIMUM_FEATURE_BYTES)
                    || !features.insert(feature.to_owned())
                {
                    return Err(invalid("deployment supported feature is invalid"));
                }
            }
        }

        Ok(Self {
            system: system.to_owned(),
            supported_features: features.into_iter().collect(),
        })
    }

    pub fn from_environment() -> io::Result<Self> {
        let system = std::env::var("TELCHAR_SYSTEM")
            .map_err(|_| invalid("TELCHAR_SYSTEM is not configured"))?;
        let features = std::env::var("TELCHAR_SUPPORTED_FEATURES").unwrap_or_default();
        Self::parse(&system, &features)
    }

    pub fn system(&self) -> &str {
        &self.system
    }

    pub fn supported_features(&self) -> &[String] {
        &self.supported_features
    }
}

fn valid_name(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
