//! Tests transfer limits contracts and failure boundaries, including finite defaults and strict parsing.

use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};

struct ManualClock(Mutex<u128>);

impl ManualClock {
    fn new() -> Self {
        Self(Mutex::new(0))
    }

    fn advance(&self, nanoseconds: u128) {
        *self.0.lock().expect("clock state available") += nanoseconds;
    }

    fn set(&self, nanoseconds: u128) {
        *self.0.lock().expect("clock state available") = nanoseconds;
    }
}

impl telchar::service::transfer_limits::MonotonicClock for ManualClock {
    fn elapsed_nanoseconds(&self) -> u128 {
        *self.0.lock().expect("clock state available")
    }
}

struct ShortWriter(Vec<u8>);

struct FailAfterWrite {
    bytes: Vec<u8>,
    failed: bool,
}

impl Write for FailAfterWrite {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.failed {
            return Err(std::io::Error::other("sink failed"));
        }
        self.bytes.push(buffer[0]);
        self.failed = true;
        Ok(1)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CountingReader {
    bytes: Vec<u8>,
    counts: Arc<Mutex<(usize, usize)>>,
}

impl CountingReader {
    fn new(bytes: Vec<u8>, counts: Arc<Mutex<(usize, usize)>>) -> Self {
        Self { bytes, counts }
    }
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut counts = self.counts.lock().expect("reader counts available");
        counts.1 += 1;
        let remaining = &self.bytes[counts.0..];
        let count = remaining.len().min(buffer.len());
        buffer[..count].copy_from_slice(&remaining[..count]);
        counts.0 += count;
        Ok(count)
    }
}

impl Write for ShortWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.push(buffer[0]);
        Ok(1)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

use telchar::service::transfer_limits::{
    LimitedReader, LimitedWriter, ObjectAdmissionState, TransferBudget, TransferLimits,
};

#[test]
fn finite_defaults_and_strict_parsing() {
    let defaults = TransferLimits::default();
    assert!(defaults.maximum_object_bytes > 0);
    assert!(defaults.maximum_inbound_session_bytes > 0);
    assert!(defaults.maximum_outbound_session_bytes > 0);
    assert_eq!(defaults.maximum_inbound_session_objects, 256);
    assert_eq!(defaults.maximum_outbound_session_objects, 256);
    assert_eq!(defaults.maximum_active_inbound_objects, 256);
    assert_eq!(defaults.maximum_active_outbound_objects, 256);
    assert_eq!(defaults.inbound_rate_bytes_per_second, 17_179_869_184);
    assert_eq!(defaults.inbound_burst_bytes, 68_719_476_736);
    assert_eq!(defaults.outbound_rate_bytes_per_second, 17_179_869_184);
    assert_eq!(defaults.outbound_burst_bytes, 68_719_476_736);

    assert!(TransferLimits::parse("0", "1", "1", "1", "1", "1", "1").is_err());
    assert!(TransferLimits::parse("not-a-number", "1", "1", "1", "1", "1", "1").is_err());
    assert!(TransferLimits::parse("18446744073709551616", "1", "1", "1", "1", "1", "1").is_err());
}

#[test]
fn rate_limits_reject_zero_malformed_negative_and_overflowing_values() {
    for value in ["0", "not-a-number", "-1", "18446744073709551616"] {
        assert!(
            TransferLimits::parse_rates(value, "1", "1", "1").is_err(),
            "{value}"
        );
    }
}

#[test]
fn inbound_rate_boundary_rejects_excess_byte_before_importer_use() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        inbound_rate_bytes_per_second: 1,
        inbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut session = TransferBudget::new(10);
    let mut reader = LimitedReader::with_rate(Cursor::new([1, 2, 3, 4]), 10, &mut session, rates);
    let mut output = Vec::new();

    let error = reader
        .read_to_end(&mut output)
        .expect_err("fourth byte exceeds inbound burst");

    assert_eq!(error.to_string(), "NAR inbound rate limit exceeded");
    assert_eq!(output, [1, 2, 3]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn exhausted_inbound_rate_uses_one_excess_probe_without_draining_source() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        inbound_rate_bytes_per_second: 1,
        inbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut session = TransferBudget::new(10);
    let counts = Arc::new(Mutex::new((0, 0)));
    let source = CountingReader::new(vec![1, 2, 3, 4, 5, 6], Arc::clone(&counts));
    let mut reader = LimitedReader::with_rate(source, 10, &mut session, rates);
    let mut output = Vec::new();

    let error = reader
        .read_to_end(&mut output)
        .expect_err("first byte above burst must reject");

    assert_eq!(error.to_string(), "NAR inbound rate limit exceeded");
    assert_eq!(output, [1, 2, 3]);
    let counts = counts.lock().expect("reader counts available");
    assert_eq!(counts.0, 4, "only one excess byte was probed");
    assert_eq!(counts.1, 2, "source was not drained after rejection");
}

#[test]
fn rate_refill_accumulates_fractional_credit_and_never_exceeds_burst() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates =
        telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock.clone());
    let mut session = TransferBudget::new(20);
    let mut drained = Vec::new();
    LimitedWriter::with_rate(&mut drained, 20, &mut session, rates.clone())
        .write_all(&[1, 2, 3])
        .expect("initial burst fits");

    for _ in 0..3 {
        clock.advance(250_000_000);
        assert!(
            rates
                .reserve_outbound(1)
                .expect("rate state available")
                .is_none(),
            "partial byte credit does not admit a byte"
        );
    }
    clock.advance(250_000_000);
    let mut reservation = rates
        .reserve_outbound(1)
        .expect("rate state available")
        .expect("four fractional advances admit one byte");
    reservation.commit(1).expect("reservation commits byte");
    drop(reservation);

    clock.set(u128::MAX);
    let mut output = Vec::new();
    LimitedWriter::with_rate(&mut output, 20, &mut session, rates.clone())
        .write_all(&[1, 2, 3])
        .expect("large elapsed refill saturates at burst");
    let error = LimitedWriter::with_rate(&mut output, 20, &mut session, rates.clone())
        .write_all(&[4])
        .expect_err("saturated bucket does not exceed burst");
    assert_eq!(error.to_string(), "NAR outbound rate limit exceeded");

    clock.set(0);
    let error = LimitedWriter::with_rate(&mut output, 20, &mut session, rates)
        .write_all(&[5])
        .expect_err("backwards clock observation grants no capacity");
    assert_eq!(error.to_string(), "NAR outbound rate limit exceeded");
}

#[test]
fn short_writes_return_unused_shared_capacity_and_charge_only_written_bytes() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut session = TransferBudget::new(10);
    let mut sink = ShortWriter(Vec::new());
    let written = LimitedWriter::with_rate(&mut sink, 10, &mut session, rates.clone())
        .write(&[1, 2, 3])
        .expect("short sink write succeeds");
    assert_eq!(written, 1);
    assert_eq!(sink.0, [1]);
    assert_eq!(session.charged(), 1);

    let mut output = Vec::new();
    LimitedWriter::with_rate(&mut output, 10, &mut session, rates)
        .write_all(&[4, 5])
        .expect("two unused reserved bytes returned");
    assert_eq!(output, [4, 5]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn completed_write_remains_charged_after_later_sink_failure() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut session = TransferBudget::new(10);
    let mut sink = FailAfterWrite {
        bytes: Vec::new(),
        failed: false,
    };
    let error = LimitedWriter::with_rate(&mut sink, 10, &mut session, rates.clone())
        .write_all(&[1, 2, 3])
        .expect_err("second sink call fails");
    assert_eq!(error.to_string(), "sink failed");
    assert_eq!(sink.bytes, [1]);
    assert_eq!(session.charged(), 1);

    let mut output = Vec::new();
    LimitedWriter::with_rate(&mut output, 10, &mut session, rates)
        .write_all(&[4, 5])
        .expect("only the successfully written byte stayed charged");
    assert_eq!(output, [4, 5]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn concurrent_reservations_cannot_exceed_shared_burst() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates =
        Arc::new(telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock));
    let first = Arc::clone(&rates);
    let second = Arc::clone(&rates);
    let first = std::thread::spawn(move || first.reserve_outbound(3).unwrap());
    let second = std::thread::spawn(move || second.reserve_outbound(3).unwrap());
    let mut reservations = [
        first.join().expect("first reservation thread completes"),
        second.join().expect("second reservation thread completes"),
    ];
    let successful = reservations
        .iter_mut()
        .filter_map(Option::take)
        .collect::<Vec<_>>();
    assert_eq!(successful.len(), 1, "reservations exceeded shared burst");
    drop(successful);
}

#[test]
fn shared_state_depletes_across_writers_but_directions_remain_independent() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        inbound_rate_bytes_per_second: 1,
        inbound_burst_bytes: 3,
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut first_session = TransferBudget::new(10);
    let mut first_output = Vec::new();
    LimitedWriter::with_rate(&mut first_output, 10, &mut first_session, rates.clone())
        .write_all(&[1, 2])
        .expect("first session consumes shared outbound capacity");

    let mut second_session = TransferBudget::new(10);
    let mut second_output = Vec::new();
    let error =
        LimitedWriter::with_rate(&mut second_output, 10, &mut second_session, rates.clone())
            .write_all(&[3, 4])
            .expect_err("second session cannot exceed remaining shared outbound capacity");
    assert_eq!(error.to_string(), "NAR outbound rate limit exceeded");
    assert_eq!(second_output, [3]);

    let mut inbound = rates
        .reserve_inbound(3)
        .expect("inbound state available")
        .expect("outbound consumption does not reduce inbound capacity");
    inbound.commit(3).expect("inbound reservation commits");
}

#[test]
fn outbound_rate_boundary_rejects_before_excess_reaches_sink() {
    let clock = Arc::new(ManualClock::new());
    let limits = TransferLimits {
        outbound_rate_bytes_per_second: 1,
        outbound_burst_bytes: 3,
        ..TransferLimits::default()
    };
    let rates = telchar::service::transfer_limits::RateAdmissionState::with_clock(&limits, clock);
    let mut session = TransferBudget::new(10);
    let mut output = Vec::new();
    let mut writer = LimitedWriter::with_rate(&mut output, 10, &mut session, rates);

    let error = writer
        .write_all(&[1, 2, 3, 4])
        .expect_err("fourth byte exceeds outbound burst");

    assert_eq!(error.to_string(), "NAR outbound rate limit exceeded");
    assert_eq!(output, [1, 2, 3]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn active_object_capacity_is_directional_and_released() {
    let limits = TransferLimits::parse("1", "1", "1", "1", "1", "1", "1").unwrap();
    let state = ObjectAdmissionState::new(&limits);
    let inbound = state.admit_inbound().expect("first inbound object fits");
    assert!(state.admit_inbound().is_err());
    assert!(state.admit_outbound().is_ok());
    drop(inbound);
    assert!(state.admit_inbound().is_ok());
}

#[test]
fn active_object_capacity_is_shared_across_threads() {
    let limits = TransferLimits::parse("1", "1", "1", "1", "1", "1", "1").unwrap();
    let state = Arc::new(ObjectAdmissionState::new(&limits));
    let held = state.admit_inbound().expect("first session holds permit");
    let contender = Arc::clone(&state);

    let rejected = std::thread::spawn(move || contender.admit_inbound().is_err())
        .join()
        .expect("contending session does not panic");

    assert!(rejected, "daemon-wide capacity was not shared");
    drop(held);
    assert!(state.admit_inbound().is_ok());
}

#[test]
fn active_object_permit_releases_during_panic_unwind() {
    let limits = TransferLimits::parse("1", "1", "1", "1", "1", "1", "1").unwrap();
    let state = ObjectAdmissionState::new(&limits);

    let unwind = std::panic::catch_unwind({
        let state = state.clone();
        move || {
            let _permit = state.admit_outbound().expect("permit acquired");
            panic!("backend panicked");
        }
    });

    assert!(unwind.is_err());
    assert!(state.admit_outbound().is_ok());
}

#[test]
fn inbound_object_stops_at_exact_boundary_and_charges_session() {
    let source = vec![1, 2, 3, 4];
    let mut session = TransferBudget::new(10);
    let mut reader = LimitedReader::new(Cursor::new(source), 3, &mut session);
    let mut output = Vec::new();

    reader
        .read_to_end(&mut output)
        .expect_err("fourth byte exceeds object limit");

    assert_eq!(output, vec![1, 2, 3]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn outbound_object_stops_before_excess_byte() {
    let mut session = TransferBudget::new(10);
    let mut output = Vec::new();
    let mut writer = LimitedWriter::new(&mut output, 3, &mut session);

    writer
        .write_all(&[1, 2, 3, 4])
        .expect_err("fourth byte exceeds object limit");

    assert_eq!(output, vec![1, 2, 3]);
    assert_eq!(session.charged(), 3);
}

#[test]
fn session_budget_rejects_at_remaining_byte_boundary() {
    let mut budget = TransferBudget::new(3);
    budget.charge(2).expect("first charge fits");
    let error = budget.charge(2).expect_err("second charge exceeds budget");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(budget.charged(), 2);
}

#[test]
fn inbound_budget_is_shared_across_objects() {
    let mut session = TransferBudget::new(5);
    let mut first_output = Vec::new();
    {
        let mut first = LimitedReader::new(Cursor::new([1, 2, 3]), 4, &mut session);
        first
            .read_to_end(&mut first_output)
            .expect("first object fits session budget");
    }

    let mut second = LimitedReader::new(Cursor::new([4, 5, 6]), 4, &mut session);
    let mut second_output = Vec::new();
    let error = second
        .read_to_end(&mut second_output)
        .expect_err("later object exceeds remaining session budget");

    assert_eq!(error.to_string(), "transfer session byte limit exceeded");
    assert_eq!(first_output, vec![1, 2, 3]);
    assert_eq!(second_output, vec![4, 5]);
    assert_eq!(session.charged(), 5);
}

#[test]
fn outbound_budget_is_shared_across_objects() {
    let mut session = TransferBudget::new(5);
    let mut first_output = Vec::new();
    {
        let mut first = LimitedWriter::new(&mut first_output, 4, &mut session);
        first
            .write_all(&[1, 2, 3])
            .expect("first object fits session budget");
    }

    let mut second_output = Vec::new();
    let mut second = LimitedWriter::new(&mut second_output, 4, &mut session);
    let error = second
        .write_all(&[4, 5, 6])
        .expect_err("later object exceeds remaining session budget");

    assert_eq!(error.to_string(), "transfer session byte limit exceeded");
    assert_eq!(first_output, vec![1, 2, 3]);
    assert_eq!(second_output, vec![4, 5]);
    assert_eq!(session.charged(), 5);
}
