//! Describes server operation boundaries without changing their results or client transport.

pub(crate) fn run<T>(phase: &'static str, operation: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    let started = std::time::Instant::now();
    tracing::info!(event = "server.phase.started", phase);
    let result = operation();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(event = "server.phase.completed", phase, elapsed_ms),
        Err(error) => tracing::warn!(event = "server.phase.failed", phase, elapsed_ms, error_kind = ?error.kind()),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    fn capture(operation: impl FnOnce()) -> String {
        let output = Output::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, operation);
        String::from_utf8(output.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn operation_success_emits_info_boundaries_and_preserves_value() {
        let logs = capture(|| {
            assert_eq!(run("queue", || Ok(42)).unwrap(), 42);
        });
        assert!(logs.contains("server.phase.started"), "{logs}");
        assert!(logs.contains("server.phase.completed"), "{logs}");
        assert!(logs.contains("phase=\"queue\""), "{logs}");
        assert!(logs.contains("elapsed_ms="), "{logs}");
        assert!(!logs.contains("server.phase.failed"), "{logs}");
    }

    #[test]
    fn operation_failure_preserves_error_without_logging_private_text() {
        let logs = capture(|| {
            let error = run::<()>("execute", || Err(io::Error::new(io::ErrorKind::BrokenPipe, "private-error-marker"))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(error.to_string(), "private-error-marker");
        });
        assert!(logs.contains("server.phase.failed"), "{logs}");
        assert!(logs.contains("BrokenPipe"), "{logs}");
        assert!(!logs.contains("private-error-marker"), "{logs}");
        assert!(!logs.contains("server.phase.completed"), "{logs}");
    }
}
