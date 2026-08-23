use std::time::Instant;

pub(super) struct DatabaseOperation {
    operation: &'static str,
    started: Instant,
}

impl DatabaseOperation {
    pub(super) fn start(operation: &'static str) -> Self {
        tracing::trace!(
            event = "database.operation.started",
            operation,
            "database operation started"
        );
        Self {
            operation,
            started: Instant::now(),
        }
    }
}

impl Drop for DatabaseOperation {
    fn drop(&mut self) {
        tracing::trace!(
            event = "database.operation.completed",
            operation = self.operation,
            duration_ms = self.started.elapsed().as_millis(),
            "database operation completed"
        );
    }
}
