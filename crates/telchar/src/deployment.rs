use std::io;
use std::time::Duration;

pub const DEFAULT_MAXIMUM_RETAINED_INPUT_BYTES: u64 = i64::MAX as u64;

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

    pub(crate) fn parse(value: &str) -> io::Result<Self> {
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
