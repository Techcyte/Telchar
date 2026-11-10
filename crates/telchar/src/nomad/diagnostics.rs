use std::error::Error;
use std::{fmt, io};

#[derive(Debug)]
struct OperationFailure {
    operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl fmt::Display for OperationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.source)
    }
}

impl Error for OperationFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(super) fn failure(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> io::Error {
    let error = OperationFailure {
        operation,
        source: Box::new(source),
    };
    tracing::warn!(
        event = "nomad.operation.failed",
        operation,
        error = %error,
        error_chain = ?error,
        "Nomad operation failed"
    );
    io::Error::other(error)
}
