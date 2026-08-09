use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

const MAXIMUM_SUPPORTED_FEATURES: usize = 64;
const MAXIMUM_SYSTEM_BYTES: usize = 64;
const MAXIMUM_FEATURE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RunningDisconnectPolicy {
    #[default]
    DetachAndFinish,
    CancelRunning,
}

impl RunningDisconnectPolicy {
    pub fn parse(value: &str) -> io::Result<Self> {
        match value {
            "detach-and-finish" => Ok(Self::DetachAndFinish),
            "cancel-running" => Ok(Self::CancelRunning),
            _ => Err(invalid("running disconnect policy is invalid")),
        }
    }

    pub fn from_environment() -> io::Result<Self> {
        match std::env::var("TELCHAR_RUNNING_DISCONNECT_POLICY") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(invalid("running disconnect policy is invalid"))
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DetachAndFinish => "detach-and-finish",
            Self::CancelRunning => "cancel-running",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputRetention {
    duration: Duration,
}

impl OutputRetention {
    pub const MINIMUM_SECONDS: u64 = 60;
    pub const DEFAULT_SECONDS: u64 = 3_600;
    pub const MAXIMUM_SECONDS: u64 = 86_400;

    pub fn from_environment() -> io::Result<Self> {
        match std::env::var("TELCHAR_OUTPUT_RETENTION_SECONDS") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(std::env::VarError::NotUnicode(_)) => Err(invalid("output retention is invalid")),
        }
    }

    pub fn duration(self) -> Duration {
        self.duration
    }

    pub fn seconds(self) -> u64 {
        self.duration.as_secs()
    }

    fn parse(value: &str) -> io::Result<Self> {
        if value.is_empty()
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid("output retention is invalid"));
        }
        let seconds = value
            .parse::<u64>()
            .map_err(|_| invalid("output retention is invalid"))?;
        if !(Self::MINIMUM_SECONDS..=Self::MAXIMUM_SECONDS).contains(&seconds) {
            return Err(invalid("output retention is invalid"));
        }
        Ok(Self {
            duration: Duration::from_secs(seconds),
        })
    }
}

impl Default for OutputRetention {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(Self::DEFAULT_SECONDS),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentConfig {
    system: String,
    supported_features: Vec<String>,
    output_retention: OutputRetention,
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
            output_retention: OutputRetention::default(),
        })
    }

    pub fn from_environment() -> io::Result<Self> {
        let system = std::env::var("TELCHAR_SYSTEM")
            .map_err(|_| invalid("TELCHAR_SYSTEM is not configured"))?;
        let features = std::env::var("TELCHAR_SUPPORTED_FEATURES").unwrap_or_default();
        let output_retention = OutputRetention::from_environment()?;
        let mut config = Self::parse(&system, &features)?;
        config.output_retention = output_retention;
        Ok(config)
    }

    pub fn system(&self) -> &str {
        &self.system
    }

    pub fn supported_features(&self) -> &[String] {
        &self.supported_features
    }

    pub fn output_retention(&self) -> OutputRetention {
        self.output_retention
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
