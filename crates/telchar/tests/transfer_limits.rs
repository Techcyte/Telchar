use std::io::{Cursor, Read, Write};

use telchar::transfer_limits::{
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

    assert!(TransferLimits::parse("0", "1", "1", "1", "1", "1", "1").is_err());
    assert!(TransferLimits::parse("not-a-number", "1", "1", "1", "1", "1", "1").is_err());
    assert!(TransferLimits::parse("18446744073709551616", "1", "1", "1", "1", "1", "1").is_err());
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
