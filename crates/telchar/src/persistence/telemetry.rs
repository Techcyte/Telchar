use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use std::sync::OnceLock;
use std::time::Instant;

struct Instruments {
    operations: Counter<u64>,
    duration: Histogram<f64>,
}

pub(super) fn run<T, E>(
    operation: &'static str,
    action: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    let instruments = INSTRUMENTS.get_or_init(|| {
        let meter = global::meter("telchar");
        Instruments {
            operations: meter
                .u64_counter("telchar.database.operations")
                .with_unit("{operation}")
                .build(),
            duration: meter
                .f64_histogram("telchar.database.operation.duration")
                .with_unit("s")
                .build(),
        }
    });
    let span = tracing::debug_span!(
        "database.operation",
        operation,
        outcome = tracing::field::Empty
    );
    let _entered = span.enter();
    let started = Instant::now();
    let result = action();
    let outcome = if result.is_ok() { "success" } else { "failure" };
    span.record("outcome", outcome);
    let attributes = [
        KeyValue::new("operation", operation),
        KeyValue::new("outcome", outcome),
    ];
    instruments
        .duration
        .record(started.elapsed().as_secs_f64(), &attributes);
    instruments.operations.add(1, &attributes);
    result
}

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
        opentelemetry::global::meter("telchar")
            .f64_histogram("telchar.database.lifecycle.duration")
            .with_unit("s")
            .build()
            .record(
                self.started.elapsed().as_secs_f64(),
                &[KeyValue::new("operation", self.operation)],
            );
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
