use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;
use telchar::nomad_transfer_authentication::{WorkloadIdentityPolicy, WorkloadIdentityVerifier};
use telchar::nomad_transfer_protocol::{Authentication, AuthenticationProof};

const MODULUS: &str = "s66-qvXhv-71C45ArMzBrmJmu7ovuAnKOXz87ZgTQtHzVQVvtznOl3Slbjp0PY1XzXQcHkLO_RrEabnCfvmmrAYgU3BemDMeYsBiG7oMc4PTAWEZbDuGTK5asV-fBz3J_6ayS_2KrqYv97_vxHuoeME_jxIW1xoZSTNv5vR2XOOLm3ecmpXf5MLcGQ9tLzEMWFPrKKpMVQKUFduk7bAL3n8FhM8fZGhvJ8W2EFvEr8eYmk7XkFa8XGdW2zfQX4221DNM8m3gkXyAETLocWijFAkwT1_bfQzIkq7682PNYSbGLiIQ8DBY1COL70TleicAgWIfLOtepFT7J34T5cPerQ";

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    aud: &'a str,
    exp: u64,
    nbf: u64,
    nomad_namespace: &'a str,
    nomad_job_id: &'a str,
    nomad_allocation_id: &'a str,
    nomad_task: &'a str,
}

fn token(allocation: &str, algorithm: Algorithm) -> String {
    let mut header = Header::new(algorithm);
    header.kid = Some("key-1".to_owned());
    encode(
        &header,
        &Claims {
            iss: "http://nomad.example:4646",
            aud: "telchar-transfer",
            exp: 1_100,
            nbf: 900,
            nomad_namespace: "telchar",
            nomad_job_id: "job-1",
            nomad_allocation_id: allocation,
            nomad_task: "build",
        },
        &EncodingKey::from_rsa_pem(include_bytes!("fixtures/nomad-workload-private.pem"))
            .expect("private key parses"),
    )
    .expect("token encodes")
}

fn authentication(token: String) -> Authentication {
    Authentication {
        backend: "nomad-primary".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        allocation_id: "allocation-1".to_owned(),
        task: "build".to_owned(),
        shared_build_digest: "digest-1".to_owned(),
        proof: AuthenticationProof::WorkloadIdentity { token },
    }
}

#[test]
fn fetches_bounded_jwks_and_verifies_exact_nomad_claims() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let jwks_url = format!(
        "http://{}/.well-known/jwks.json",
        listener.local_addr().expect("address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("JWKS request accepts");
        let mut request = [0_u8; 1024];
        let length = stream.read(&mut request).expect("request reads");
        assert!(String::from_utf8_lossy(&request[..length])
            .starts_with("GET /.well-known/jwks.json HTTP/1.1\r\n"));
        let body = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"key-1","use":"sig","alg":"RS256","n":"{MODULUS}","e":"AQAB"}}]}}"#
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("response writes");
    });
    let verifier = WorkloadIdentityVerifier::new(WorkloadIdentityPolicy {
        issuer: "http://nomad.example:4646".to_owned(),
        jwks_url,
        audience: "telchar-transfer".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        task: "build".to_owned(),
        ca_certificate_file: None,
        request_timeout: Duration::from_secs(5),
        maximum_jwks_bytes: 16 * 1024,
        clock_skew: Duration::from_secs(30),
    })
    .expect("verifier creates");

    verifier
        .verify(
            &authentication(token("allocation-1", Algorithm::RS256)),
            UNIX_EPOCH + Duration::from_secs(1_000),
        )
        .expect("identity verifies");
    server.join().expect("server joins");
}

#[test]
fn rejects_foreign_claims_and_unsupported_algorithm() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let jwks_url = format!(
        "http://{}/.well-known/jwks.json",
        listener.local_addr().expect("address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("JWKS request accepts");
        let mut request = [0_u8; 1024];
        let _length = stream.read(&mut request).expect("request reads");
        let body = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"key-1","use":"sig","alg":"RS256","n":"{MODULUS}","e":"AQAB"}}]}}"#
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("response writes");
    });
    let policy = WorkloadIdentityPolicy {
        issuer: "http://nomad.example:4646".to_owned(),
        jwks_url,
        audience: "telchar-transfer".to_owned(),
        namespace: "telchar".to_owned(),
        job_id: "job-1".to_owned(),
        task: "build".to_owned(),
        ca_certificate_file: None,
        request_timeout: Duration::from_millis(50),
        maximum_jwks_bytes: 16 * 1024,
        clock_skew: Duration::from_secs(30),
    };
    let verifier = WorkloadIdentityVerifier::new(policy).expect("verifier creates");
    assert!(verifier
        .verify(
            &authentication(token("foreign", Algorithm::RS256)),
            UNIX_EPOCH + Duration::from_secs(1_000),
        )
        .is_err());
    server.join().expect("server joins");

    let valid = token("allocation-1", Algorithm::RS256);
    let (_, remainder) = valid.split_once('.').expect("token header exists");
    let unsupported_header =
        URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT","kid":"key-1"}"#);
    let unsupported = format!("{unsupported_header}.{remainder}");
    assert!(verifier
        .verify(
            &authentication(unsupported),
            UNIX_EPOCH + Duration::from_secs(1_000),
        )
        .is_err());
}
