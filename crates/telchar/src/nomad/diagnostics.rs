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

pub(crate) fn transient_cleanup_failure(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::TimedOut {
        return true;
    }
    let mut source: Option<&(dyn Error + 'static)> = error.get_ref().map(|error| error as _);
    while let Some(error) = source {
        if let Some(request) = error.downcast_ref::<reqwest::Error>() {
            return request.is_timeout()
                || request
                    .status()
                    .is_some_and(|status| matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504));
        }
        source = error.source();
    }
    false
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
