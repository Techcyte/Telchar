use std::process::Command;

use telchar::worker_trace::TraceCapture;

#[test]
fn captures_real_nix_worker_handshake_and_operation_without_payloads() {
    let capture = TraceCapture::start("/nix/var/nix/daemon-socket/socket").expect("capture starts");

    let output = Command::new("nix")
        .args(["--store", &capture.store_url(), "store", "info", "--json"])
        .output()
        .expect("real Nix client runs");
    assert!(output.status.success(), "Nix client failed: {output:?}");

    let trace = capture.finish().expect("capture finishes");
    assert_eq!(trace.client_protocol_version(), (1, 38));
    assert_eq!(trace.peer_protocol_version(), (1, 38));
    assert_eq!(trace.operations(), &[19]);
    assert!(!trace.contains_payloads());
    assert_eq!(
        trace.sanitized_json(),
        "{\"client_protocol\":\"1.38\",\"operations\":[19],\"peer_protocol\":\"1.38\"}"
    );
}
