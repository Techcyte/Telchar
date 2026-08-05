#![forbid(unsafe_code)]

pub fn protocol_name() -> &'static str {
    "Nix worker protocol"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    CleanEof,
    Truncated,
    SizeLimit,
    UnsupportedOperation,
    VersionMismatch,
    StoreFailure,
    InternalFailure,
}

#[cfg(test)]
mod tests {
    use super::{ProtocolError, protocol_name};

    #[test]
    fn reports_protocol_name() {
        assert_eq!(protocol_name(), "Nix worker protocol");
    }

    #[test]
    fn distinguishes_protocol_failure_classes() {
        let cases = [
            ProtocolError::CleanEof,
            ProtocolError::Truncated,
            ProtocolError::SizeLimit,
            ProtocolError::UnsupportedOperation,
            ProtocolError::VersionMismatch,
            ProtocolError::StoreFailure,
            ProtocolError::InternalFailure,
        ];

        assert_eq!(cases.len(), 7);
    }
}
