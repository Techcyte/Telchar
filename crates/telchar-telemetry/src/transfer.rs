//! Aggregates synchronous stream I/O cost without emitting per-buffer telemetry.

use opentelemetry::{KeyValue, global};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum TransferSide {
    Source,
    Destination,
}

/// One stream's observed calls, including blocking, copying and local processing.
pub struct TransferIo<T> {
    inner: T,
    side: TransferSide,
    elapsed: Duration,
    calls: u64,
    bytes: u64,
    failed: bool,
}

impl<T> TransferIo<T> {
    pub fn new(inner: T, side: TransferSide) -> Self {
        Self {
            inner,
            side,
            elapsed: Duration::ZERO,
            calls: 0,
            bytes: 0,
            failed: false,
        }
    }

    fn record(&mut self, started: Instant, result: &io::Result<usize>) {
        self.elapsed += started.elapsed();
        self.calls += 1;
        match result {
            Ok(bytes) => self.bytes += *bytes as u64,
            Err(_) => self.failed = true,
        }
    }
}

impl<T: Read> Read for TransferIo<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let started = Instant::now();
        let result = self.inner.read(buffer);
        self.record(started, &result);
        result
    }
}

impl<T: Write> Write for TransferIo<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let started = Instant::now();
        let result = self.inner.write(buffer);
        self.record(started, &result);
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        let started = Instant::now();
        let result = self.inner.flush();
        self.elapsed += started.elapsed();
        self.calls += 1;
        self.failed |= result.is_err();
        result
    }
}

impl<T> Drop for TransferIo<T> {
    fn drop(&mut self) {
        let side = match self.side {
            TransferSide::Source => "source",
            TransferSide::Destination => "destination",
        };
        let outcome = if self.failed { "failure" } else { "success" };
        let attributes = [
            KeyValue::new("side", side),
            KeyValue::new("outcome", outcome),
        ];
        let meter = global::meter("telchar-transfer");
        meter
            .f64_histogram("telchar.transfer.io.duration")
            .with_unit("s")
            .build()
            .record(self.elapsed.as_secs_f64(), &attributes);
        meter
            .u64_counter("telchar.transfer.io.calls")
            .with_unit("{call}")
            .build()
            .add(self.calls, &attributes);
        meter
            .u64_counter("telchar.transfer.io.bytes")
            .with_unit("By")
            .build()
            .add(self.bytes, &attributes);
        tracing::debug!(
            event = "transfer.io.summary",
            side,
            outcome,
            calls = self.calls,
            bytes = self.bytes,
            total_seconds = self.elapsed.as_secs_f64()
        );
    }
}
