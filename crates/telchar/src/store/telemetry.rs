//! Records bounded daemon operations independently of diagnostic log filtering.

use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};
use opentelemetry::{KeyValue, global};
use std::sync::OnceLock;
use std::time::Instant;

struct Instruments {
    operations: Counter<u64>,
    duration: Histogram<f64>,
    active: UpDownCounter<i64>,
}

fn instruments() -> &'static Instruments {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(|| {
        let meter = global::meter("telchar");
        Instruments {
            operations: meter
                .u64_counter("telchar.store.operations")
                .with_unit("{operation}")
                .build(),
            duration: meter
                .f64_histogram("telchar.store.operation.duration")
                .with_unit("s")
                .build(),
            active: meter
                .i64_up_down_counter("telchar.store.operations.active")
                .with_unit("{operation}")
                .build(),
        }
    })
}

struct Operation {
    name: &'static str,
    started: Instant,
    span: tracing::Span,
    outcome: &'static str,
}

impl Drop for Operation {
    fn drop(&mut self) {
        let instruments = instruments();
        let terminal = [
            KeyValue::new("operation", self.name),
            KeyValue::new("outcome", self.outcome),
        ];
        self.span.record("outcome", self.outcome);
        instruments
            .duration
            .record(self.started.elapsed().as_secs_f64(), &terminal);
        instruments.operations.add(1, &terminal);
        instruments
            .active
            .add(-1, &[KeyValue::new("operation", self.name)]);
    }
}

pub(super) fn run<T, E>(name: &'static str, action: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let span = tracing::info_span!(
        "store.operation",
        operation = name,
        outcome = tracing::field::Empty
    );
    instruments()
        .active
        .add(1, &[KeyValue::new("operation", name)]);
    let mut operation = Operation {
        name,
        started: Instant::now(),
        span,
        outcome: "failure",
    };
    let entered = operation.span.enter();
    let result = action();
    drop(entered);
    if result.is_ok() {
        operation.outcome = "success";
    }
    result
}
