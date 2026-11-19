//! Verifies aggregated I/O measurements preserve streaming behavior and errors.

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;
use std::io::{self, Read, Write};
use telchar_telemetry::{TransferIo, TransferSide};

struct Rejected;
impl Write for Rejected {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn exports_one_summary_per_stream_with_exact_bytes_and_failures() {
    let name = "exports_one_summary_per_stream_with_exact_bytes_and_failures";
    if !telchar_test_support::process::is_child(name) {
        let collector = collector::start_collector();
        telchar_test_support::process::assert_success(
            telchar_test_support::process::command(name)
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
                .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        );
        collector.assert_sum_attributes(
            "telchar.transfer.io.bytes",
            &[("side", "source"), ("outcome", "success")],
            7,
        );
        collector.assert_sum_attributes(
            "telchar.transfer.io.bytes",
            &[("side", "destination"), ("outcome", "success")],
            7,
        );
        collector.assert_sum_attributes(
            "telchar.transfer.io.bytes",
            &[("side", "destination"), ("outcome", "failure")],
            0,
        );
        collector.assert_histogram_attributes(
            "telchar.transfer.io.duration",
            &[("side", "destination"), ("outcome", "failure")],
            0.0,
        );
        collector.assert_no_private_markers(&["payload", "secret"]);
        collector.assert_metric_attributes_are_bounded();
        return;
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("transfer-test", "test").unwrap();
    let mut output = Vec::new();
    {
        let mut source = TransferIo::new(&b"payload"[..], TransferSide::Source);
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes).unwrap();
        let mut destination = TransferIo::new(&mut output, TransferSide::Destination);
        destination.write_all(&bytes).unwrap();
        destination.flush().unwrap();
    }
    assert_eq!(output, b"payload");
    {
        let mut rejected = TransferIo::new(Rejected, TransferSide::Destination);
        assert_eq!(
            rejected.write_all(b"secret").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
    telemetry.shutdown();
}
