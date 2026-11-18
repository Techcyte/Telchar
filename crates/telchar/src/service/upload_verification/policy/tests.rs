use super::*;
use serde_json::json;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::thread;

fn command(script: &str) -> (tempfile::TempDir, PolicyHook) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("policy");
    fs::write(&path, format!("#!/usr/bin/env bash\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let hook = PolicyHook {
        command: Some(vec![path.to_str().unwrap().into()]),
        url: None,
        timeout: "1s".into(),
    };
    (root, hook)
}

#[test]
fn configuration_requires_one_bounded_transport_and_positive_timeout() {
    let hook: PolicyHook = serde_json::from_value(json!({"command": ["/policy"]})).unwrap();
    assert_eq!(hook.timeout, "2s");
    hook.validate().unwrap();
    assert!(
        serde_json::from_value::<PolicyHook>(json!({"command": ["/policy"], "typo": true}))
            .is_err()
    );
    for value in [
        json!({}),
        json!({"command": ["/policy"], "url": "http://localhost/"}),
        json!({"command": []}),
        json!({"command": ["relative"]}),
        json!({"command": ["/policy", "nul\u{0000}argument"]}),
        json!({"command": ["/policy", "x".repeat(4097)]}),
        json!({"command": vec!["/policy"; 65]}),
        json!({"url": "file:///policy"}),
        json!({"url": "http://user:secret@localhost/"}),
        json!({"url": "https://localhost/#fragment"}),
        json!({"url": "https://localhost/", "timeout": "0s"}),
        json!({"url": "https://localhost/", "timeout": "25h"}),
        json!({"url": "https://localhost/", "timeout": "1ms"}),
    ] {
        let hook: PolicyHook = serde_json::from_value(value).unwrap();
        assert!(hook.validate().is_err(), "invalid configuration accepted");
    }
    for url in ["http://localhost/policy", "https://localhost/policy"] {
        let hook: PolicyHook = serde_json::from_value(json!({"url": url})).unwrap();
        hook.validate().unwrap();
    }
}

#[test]
fn command_receives_json_and_returns_allow_or_deny() {
    let (root, mut hook) =
        command("IFS= read -r request\nprintf '%s' \"$request\" > \"$1\"\nprintf '%s' \"$2\"");
    let captured = root.path().join("request.json");
    let request = sample_request();
    for allow in [true, false] {
        let decision = json!({"allow": allow, "reason": "operator policy"});
        hook.command.as_mut().unwrap().truncate(1);
        hook.command
            .as_mut()
            .unwrap()
            .extend([captured.to_str().unwrap().into(), decision.to_string()]);
        let result = hook.invoke(&request).unwrap();
        assert_eq!(result.allow, allow);
        assert_eq!(result.reason.as_deref(), Some("operator policy"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&captured).unwrap()).unwrap(),
            request
        );
        assert_eq!(serde_json::to_value(&result).unwrap(), decision);
    }
}

#[test]
fn policy_reasons_reject_oversize_and_control_characters() {
    let (_root, mut hook) = command("cat >/dev/null; printf '%s' \"$1\"");
    for reason in ["x".repeat(4097), "line\nbreak".into()] {
        hook.command.as_mut().unwrap().truncate(1);
        hook.command
            .as_mut()
            .unwrap()
            .push(json!({"allow":false,"reason":reason}).to_string());
        assert!(hook.invoke(&sample_request()).is_err());
    }
}

#[test]
fn command_failure_and_malformed_decisions_fail_closed_without_diagnostics() {
    for script in [
        "cat >/dev/null; printf '%s' '{\"allow\":true}'; printf 'SECRET' >&2; exit 9",
        "cat >/dev/null; printf '%s' '{\"reason\":\"SECRET\"}'",
        "cat >/dev/null; printf '%s' '{\"allow\":\"true\"}'",
        "cat >/dev/null; printf '%s' '{\"allow\":true} trailing SECRET'",
        "cat >/dev/null; printf SECRET",
        "kill -KILL $$",
    ] {
        let (_root, hook) = command(script);
        let error = hook.invoke(&json!({})).unwrap_err().to_string();
        assert!(!error.contains("SECRET"));
    }
    let (_root, mut hook) = command("exit 0");
    hook.command = Some(vec!["/nonexistent-telchar-policy-command".into()]);
    assert!(hook.invoke(&json!({})).is_err());
}

#[test]
fn command_response_limit_is_exact_and_stderr_is_discarded() {
    let (_root, hook) = command(
        "cat >/dev/null; printf '%s' '{\"allow\":true}'; printf '%*s' 65522 ''; printf '%*s' 131072 '' >&2",
    );
    assert!(hook.invoke(&json!({})).unwrap().allow);
    let (_root, hook) = command("cat >/dev/null; printf '%*s' 65537 ''");
    assert!(hook.invoke(&json!({})).is_err());
}

#[test]
fn command_deadline_covers_blocked_stdin_noisy_stderr_and_inherited_pipes() {
    for (script, request) in [
        (
            "while :; do sleep 1; done",
            json!({"large": "x".repeat(4 * 1024 * 1024)}),
        ),
        (
            "cat >/dev/null; while :; do printf '%*s' 4096 '' >&2; done",
            json!({}),
        ),
        (
            "cat >/dev/null; sleep 30 & printf '%s' '{\"allow\":true}'",
            json!({}),
        ),
    ] {
        let (_root, hook) = command(script);
        let started = Instant::now();
        assert_eq!(
            hook.invoke(&request).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}

#[test]
fn command_timeout_kills_descendants() {
    let (root, mut hook) =
        command("(sleep 2; printf leaked > \"$1\") &\nwhile :; do sleep 1; done");
    let marker = root.path().join("descendant-survived");
    hook.command
        .as_mut()
        .unwrap()
        .push(marker.to_str().unwrap().into());
    assert_eq!(
        hook.invoke(&json!({})).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    thread::sleep(Duration::from_millis(1500));
    assert!(!marker.exists(), "policy descendant survived timeout");
}

fn sample_request() -> serde_json::Value {
    json!({
        "version": 1,
        "token": {"sub": "alice"},
        "upload": {
            "storePath": "/nix/store/00000000000000000000000000000000-output",
            "narHash": format!("sha256:{}", "ab".repeat(32)),
            "narSize": 123,
            "references": [],
            "contentAddress": null,
            "alreadyValid": false
        },
        "signatures": [{
            "keyName": "ci-1", "status": "valid", "source": "static",
            "metadata": {"identity": "alice", "kind": "ci"}
        }]
    })
}

fn read_request(stream: &mut TcpStream) -> serde_json::Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 16384);
    }
    let headers = String::from_utf8(headers).unwrap();
    assert!(headers.starts_with("POST /policy HTTP/1.1\r\n"));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn http_hook(
    status: &str,
    extra_headers: &str,
    body: Vec<u8>,
    header_delay: Duration,
    body_delay: Duration,
) -> (PolicyHook, thread::JoinHandle<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
        body.len()
    );
    let server = thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(5),
                        "no policy request"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        };
        stream
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let request = read_request(&mut stream);
        thread::sleep(header_delay);
        let _ = stream.write_all(headers.as_bytes());
        thread::sleep(body_delay);
        let _ = stream.write_all(&body);
        request
    });
    (
        PolicyHook {
            command: None,
            url: Some(format!("http://{address}/policy")),
            timeout: "1s".into(),
        },
        server,
    )
}

#[test]
fn http_posts_json_and_obeys_allow_and_deny() {
    for allow in [true, false] {
        let (hook, server) = http_hook(
            "200 OK",
            "",
            json!({"allow": allow}).to_string().into_bytes(),
            Duration::ZERO,
            Duration::ZERO,
        );
        let request = sample_request();
        assert_eq!(hook.invoke(&request).unwrap().allow, allow);
        assert_eq!(server.join().unwrap(), request);
    }
}

#[test]
fn http_rejects_non_success_oversize_and_missing_allow() {
    for (status, headers, body) in [
        ("403 Forbidden", "", b"SECRET".to_vec()),
        ("200 OK", "", vec![b' '; MAXIMUM_RESPONSE_BYTES + 1]),
        ("200 OK", "", b"{\"reason\":\"SECRET\"}".to_vec()),
    ] {
        let (hook, server) = http_hook(status, headers, body, Duration::ZERO, Duration::ZERO);
        let error = hook.invoke(&json!({})).unwrap_err().to_string();
        assert!(!error.contains("SECRET"));
        server.join().unwrap();
    }
}

#[test]
fn http_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < Duration::from_secs(5));
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        };
        read_request(&mut stream);
        write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{address}/policy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(250) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"allow\":true}");
                    return true;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        }
        false
    });
    let hook = PolicyHook {
        command: None,
        url: Some(format!("http://{address}/policy")),
        timeout: "1s".into(),
    };
    let result = hook.invoke(&json!({}));
    assert!(!server.join().unwrap(), "policy redirect was followed");
    assert!(result.is_err());
}

#[test]
fn http_caps_body_at_exact_limit() {
    let mut body = b"{\"allow\":true}".to_vec();
    body.resize(MAXIMUM_RESPONSE_BYTES, b' ');
    let (hook, server) = http_hook("200 OK", "", body, Duration::ZERO, Duration::ZERO);
    assert!(hook.invoke(&json!({})).unwrap().allow);
    server.join().unwrap();
}

#[test]
fn http_deadline_covers_headers_and_body() {
    for (header_delay, body_delay) in [
        (Duration::from_millis(1400), Duration::ZERO),
        (Duration::ZERO, Duration::from_millis(1400)),
    ] {
        let (hook, server) = http_hook(
            "200 OK",
            "",
            b"{\"allow\":true}".to_vec(),
            header_delay,
            body_delay,
        );
        let started = Instant::now();
        assert!(hook.invoke(&json!({})).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        server.join().unwrap();
    }
}
