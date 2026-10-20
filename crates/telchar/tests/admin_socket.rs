//! Checks the administrative socket's kernel identity and bounded transport.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use telchar::service::admin::{Request, read_request};

#[test]
fn administrative_requests_reject_claimed_actor_and_frontend_envelopes() {
    for bytes in [
        br#"{"action":"risk-release","intent_id":"intent","provider_id":null,"reason":"incident","evidence_reference":"ticket:1","accept_remote_execution_risk":true,"actor_uid":0}"#.as_slice(),
        b"TIPC\x02\x00".as_slice(),
    ] {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        client.write_all(bytes).unwrap();
        assert!(read_request(&mut server, Duration::from_secs(1)).is_err());
    }
}

#[test]
fn administrative_frames_have_size_and_absolute_time_bounds() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert!(read_request(&mut server, Duration::from_secs(1)).is_err());

    let (_client, mut server) = UnixStream::pair().unwrap();
    let start = Instant::now();
    assert!(read_request(&mut server, Duration::from_millis(50)).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn administrative_request_round_trip_preserves_exact_identity() {
    let request = Request::RiskRelease {
        intent_id: "intent".into(),
        provider_id: Some("microvm-exact".into()),
        reason: "Remote execution risk accepted".into(),
        evidence_reference: "incident:119".into(),
        accept_remote_execution_risk: true,
    };
    let (mut client, mut server) = UnixStream::pair().unwrap();
    telchar::service::admin::write_request(&mut client, &request).unwrap();
    assert_eq!(
        read_request(&mut server, Duration::from_secs(1)).unwrap(),
        request
    );
}

#[test]
fn nonroot_peer_cannot_reach_administrative_request_decoding() {
    assert_ne!(unsafe { libc::geteuid() }, 0, "run fixture as nonroot");
    let (_client, server) = UnixStream::pair().unwrap();
    let error = telchar::service::admin::authorize(&server).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}
