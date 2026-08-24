use std::time::Instant;

pub(super) struct DatabaseOperation {
    operation: &'static str,
    started: Instant,
    emit: bool,
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
            emit: true,
        }
    }

    /// Defers trace emission so no-op maintenance polls remain silent.
    pub(super) fn silent(operation: &'static str) -> Self {
        Self {
            operation,
            started: Instant::now(),
            emit: false,
        }
    }

    /// Emits the deferred start event when an operation produces actionable work.
    pub(super) fn emit(&mut self) {
        if self.emit {
            return;
        }
        tracing::trace!(
            event = "database.operation.started",
            operation = self.operation,
            "database operation started"
        );
        self.emit = true;
    }
}

impl Drop for DatabaseOperation {
    fn drop(&mut self) {
        if !self.emit {
            return;
        }
        tracing::trace!(
            event = "database.operation.completed",
            operation = self.operation,
            duration_ms = self.started.elapsed().as_millis(),
            "database operation completed"
        );
    }
}
