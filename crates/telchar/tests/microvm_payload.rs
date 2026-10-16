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
    let mut process = Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg("-c")
        .arg(
            r#"import json, pathlib, sys, tempfile
sys.path.insert(0, sys.argv[1])
from execution import ExecutionState
payload = sys.stdin.read()
with tempfile.TemporaryDirectory() as directory:
    destination = pathlib.Path(directory) / 'binding'
    state = ExecutionState([sys.executable, '-c', 'import time; time.sleep(30)'],
                           [sys.executable, '-c', 'raise SystemExit(0)'],
                           [sys.executable, '-c', 'raise SystemExit(0)'], destination)
    assert state.start_hook({'microvmId': 'provider-exact', 'runHookPayload': payload})
    state.join()
    assert state.status() == 'succeeded'
    assert destination.stat().st_mode & 0o777 == 0o600
    print(destination.read_text())
"#,
        )
        .arg(root.join("deploy/microvm"))
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
            "bearer": capability.bearer(), "lifetime_seconds": 5
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
