use telchar::nomad_transfer_protocol::{
    BuildOutcome, BuildResultMetadata, NarMetadata, OutputReceipt, OutputTransferSession,
};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn output(name: &str) -> String {
    format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{name}")
}

#[test]
fn completes_only_after_every_exact_output_is_received_and_accepted() {
    let first = output("first");
    let second = output("second");
    let mut session =
        OutputTransferSession::new(vec![first.clone(), second.clone()], 1024, 2048, 256)
            .expect("session creates");

    session
        .declare(NarMetadata {
            path: first.clone(),
            nar_hash: HASH.to_owned(),
            nar_size: 10,
        })
        .expect("first output declares");
    session.receive_nar(&first, 10).expect("first NAR receives");
    session
        .record_receipt(OutputReceipt {
            path: first.clone(),
            accepted: true,
        })
        .expect("first receipt records");
    assert!(session
        .finish(&BuildResultMetadata {
            outcome: BuildOutcome::Built,
            diagnostic: None,
        })
        .is_err());

    session
        .declare(NarMetadata {
            path: second.clone(),
            nar_hash: HASH.to_owned(),
            nar_size: 20,
        })
        .expect("second output declares");
    session
        .receive_nar(&second, 20)
        .expect("second NAR receives");
    session
        .record_receipt(OutputReceipt {
            path: second,
            accepted: true,
        })
        .expect("second receipt records");
    session
        .finish(&BuildResultMetadata {
            outcome: BuildOutcome::Built,
            diagnostic: None,
        })
        .expect("exact terminal success accepts");
    assert!(session.is_complete());
}

#[test]
fn rejects_foreign_duplicate_oversized_and_out_of_order_outputs() {
    let expected = output("expected");
    let mut session =
        OutputTransferSession::new(vec![expected.clone()], 16, 16, 16).expect("session creates");
    assert!(session
        .declare(NarMetadata {
            path: output("foreign"),
            nar_hash: HASH.to_owned(),
            nar_size: 1,
        })
        .is_err());
    assert!(session.receive_nar(&expected, 1).is_err());
    assert!(session
        .declare(NarMetadata {
            path: expected.clone(),
            nar_hash: HASH.to_owned(),
            nar_size: 17,
        })
        .is_err());

    session
        .declare(NarMetadata {
            path: expected.clone(),
            nar_hash: HASH.to_owned(),
            nar_size: 16,
        })
        .expect("expected output declares");
    assert!(session
        .declare(NarMetadata {
            path: expected.clone(),
            nar_hash: HASH.to_owned(),
            nar_size: 16,
        })
        .is_err());
    assert!(session.receive_nar(&expected, 15).is_err());
    session
        .receive_nar(&expected, 16)
        .expect("exact NAR receives");
    assert!(session.receive_nar(&expected, 16).is_err());
    assert!(session
        .record_receipt(OutputReceipt {
            path: expected.clone(),
            accepted: false,
        })
        .is_err());
    session
        .record_receipt(OutputReceipt {
            path: expected,
            accepted: true,
        })
        .expect("accepted receipt records");
}

#[test]
fn permits_bounded_terminal_failure_without_outputs() {
    let mut session =
        OutputTransferSession::new(vec![output("expected")], 16, 16, 16).expect("session creates");
    session
        .finish(&BuildResultMetadata {
            outcome: BuildOutcome::Failed,
            diagnostic: Some("builder failed".to_owned()),
        })
        .expect("bounded failure accepts");
    assert!(session.is_complete());

    let mut oversized =
        OutputTransferSession::new(vec![output("expected")], 16, 16, 16).expect("session creates");
    assert!(oversized
        .finish(&BuildResultMetadata {
            outcome: BuildOutcome::Failed,
            diagnostic: Some("x".repeat(17)),
        })
        .is_err());
}
