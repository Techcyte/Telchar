//! Checks gateway-generated hooks against the image runtime's actual execution boundary.

use std::io::Write;
use std::process::{Command, Stdio};
use telchar::microvm::Capability;

#[test]
fn gateway_payload_runs_image_hook_with_exact_capability_and_identity() {
    let capability = Capability::generate().unwrap();
    let payload = capability
        .launch_payload(
            "wss://gateway.example/microvm",
            "127.0.0.1:443".parse().unwrap(),
            "intent-exact",
            5,
        )
        .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut process = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--propagation",
            "private",
            "python3",
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg("-c")
        .arg(
            r#"import json, pathlib, sys, tempfile
sys.path.insert(0, sys.argv[1])
from execution import ExecutionState
payload = sys.stdin.read()
trust = pathlib.Path(sys.argv[2])
with tempfile.TemporaryDirectory() as directory:
    destination = pathlib.Path(directory) / 'binding'
    captured = pathlib.Path(directory) / 'captured'
    worker = "import pathlib,sys; p=pathlib.Path(sys.argv[1]); assert p.stat().st_mode & 0o777 == 0o600; pathlib.Path(sys.argv[2]).write_text(p.read_text())"
    state = ExecutionState([sys.executable, '-c', 'import time; time.sleep(30)'],
                           [sys.executable, '-c', worker, str(destination), str(captured)],
                           [sys.executable, '-c', 'raise SystemExit(0)'], destination, trust)
    assert state.start_hook({'microvmId': 'provider-exact', 'runHookPayload': payload})
    state.join()
    assert state.status() == 'succeeded'
    assert not destination.exists()
    print(captured.read_text())
"#,
        )
        .arg(root.join("deploy/microvm"))
        .arg(root.join("crates/telchar-nomad-worker/tests/fixtures/microvm-cert.der"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let binding: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        binding,
        serde_json::json!({
            "endpoint": "wss://gateway.example/microvm", "address": "127.0.0.1:443",
            "intent_id": "intent-exact", "provider_id": "provider-exact",
            "assignment_epoch": 1, "bearer": capability.bearer(), "trace_context": {"traceparent": null, "tracestate": null}, "lifetime_seconds": 5
        })
    );
}

#[test]
fn gateway_payload_rejects_unusable_execution_coordinates() {
    let capability = Capability::generate().unwrap();
    for endpoint in [
        "http://gateway.example/microvm",
        "wss://gateway.example/other",
        "wss://user@gateway.example/microvm",
        "wss://gateway.example/microvm?",
        "wss://gateway.example/microvm#",
        "wss://gateway.example:0/microvm",
    ] {
        assert!(
            capability
                .launch_payload(endpoint, "127.0.0.1:443".parse().unwrap(), "intent", 5)
                .is_err()
        );
    }
    for (address, intent, lifetime) in [
        ("127.0.0.1:0", "intent", 5),
        ("0.0.0.0:443", "intent", 5),
        ("127.0.0.1:443", "", 5),
        ("127.0.0.1:443", "intent", 0),
        ("127.0.0.1:443", "intent", 28801),
    ] {
        assert!(
            capability
                .launch_payload(
                    "wss://gateway.example/microvm",
                    address.parse().unwrap(),
                    intent,
                    lifetime
                )
                .is_err()
        );
    }
    assert!(
        capability
            .launch_payload(
                "wss://gateway.example/microvm",
                "[::1]:443".parse().unwrap(),
                &"i".repeat(129),
                1
            )
            .is_err()
    );
}

#[test]
fn assignment_trace_carrier_requires_exact_nullable_fields_and_valid_context() {
    use telchar::microvm::AssignmentTraceContext;
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"traceparent": null}),
        serde_json::json!({"traceparent": null, "tracestate": null, "baggage": "private"}),
    ] {
        assert!(serde_json::from_value::<AssignmentTraceContext>(invalid).is_err());
    }
    for (parent, state) in [
        (Some("bad".to_owned()), None),
        (None, Some("vendor=value".to_owned())),
        (Some("x".repeat(56)), None),
        (
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned()),
            Some("x".repeat(513)),
        ),
    ] {
        let carrier = serde_json::from_value::<AssignmentTraceContext>(
            serde_json::json!({"traceparent": parent, "tracestate": state}),
        )
        .unwrap();
        assert!(carrier.context().is_err());
    }
    let carrier = serde_json::from_value::<AssignmentTraceContext>(
        serde_json::json!({"traceparent": null, "tracestate": null}),
    )
    .unwrap();
    assert_eq!(
        carrier.context().unwrap(),
        telchar_telemetry::TraceContext::default()
    );
}
