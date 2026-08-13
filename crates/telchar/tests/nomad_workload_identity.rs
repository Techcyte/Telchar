use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, decode_header};
use telchar::nomad_callback::AuthenticationVerifier;
use telchar::nomad_transfer_authentication::{WorkloadIdentityPolicy, WorkloadIdentityVerifier};
use telchar::nomad_transfer_protocol::{Authentication, AuthenticationProof};

const MODULUS: &str = "s66-qvXhv-71C45ArMzBrmJmu7ovuAnKOXz87ZgTQtHzVQVvtznOl3Slbjp0PY1XzXQcHkLO_RrEabnCfvmmrAYgU3BemDMeYsBiG7oMc4PTAWEZbDuGTK5asV-fBz3J_6ayS_2KrqYv97_vxHuoeME_jxIW1xoZSTNv5vR2XOOLm3ecmpXf5MLcGQ9tLzEMWFPrKKpMVQKUFduk7bAL3n8FhM8fZGhvJ8W2EFvEr8eYmk7XkFa8XGdW2zfQX4221DNM8m3gkXyAETLocWijFAkwT1_bfQzIkq7682PNYSbGLiIQ8DBY1COL70TleicAgWIfLOtepFT7J34T5cPerQ";

const VALID_TOKEN: &str = concat!(
    "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImtleS0xIn0.",
    "eyJpc3MiOiJodHRwOi8vbm9tYWQuZXhhbXBsZTo0NjQ2IiwiYXVkIjoidGVsY2hhci10cmFuc2ZlciIsImV4cCI6MTEwMCwibmJmIjo5MDAsIm5vbWFkX25hbWVzcGFjZSI6InRlbGNoYXIiLCJub21hZF9qb2JfaWQiOiJqb2ItMSIsIm5vbWFkX2FsbG9jYXRpb25faWQiOiJhbGxvY2F0aW9uLTEiLCJub21hZF90YXNrIjoiYnVpbGQifQ.",
    "lfrDiO2U7LepZaw0zWkpPxymkfZ_gXH2zyeSHeiXncpI3-oCHfihb_fnQSve6xOH38uK1ivs-XvfU_oCbfuRhs4YXlytC3JDVo-Hnb6wqubcWk94lhJCCbZpLwa9qISCPaB59qcJeA4RDQQHJKk7mDJ4MbGyBTJqxb1pASm76ygq75MV9N2aAueR_jh2VWJ2ih1WjpWWxN2-nTGQpa50krgdCwTzzcTux47EdTwGKzLsHciu5c0s5uhksE2_5FJNPZblg6o91gQAinchmHyoQ0ewUz6vu3RW3xYCxtX9kxM2GN_kHykJGAC6M7b2HY-43PA4v3fDYErLWUYeUe2eEQ"
);
const FOREIGN_TOKEN: &str = concat!(
    "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImtleS0xIn0.",
    "eyJpc3MiOiJodHRwOi8vbm9tYWQuZXhhbXBsZTo0NjQ2IiwiYXVkIjoidGVsY2hhci10cmFuc2ZlciIsImV4cCI6MTEwMCwibmJmIjo5MDAsIm5vbWFkX25hbWVzcGFjZSI6InRlbGNoYXIiLCJub21hZF9qb2JfaWQiOiJqb2ItMSIsIm5vbWFkX2FsbG9jYXRpb25faWQiOiJmb3JlaWduIiwibm9tYWRfdGFzayI6ImJ1aWxkIn0.",
    "M43k2cVVTFvrX1PzUmCtvMmXoT-_UoxOwVtJ0920s5c7JItmSiz7TRrZ9WJ_mmnXjlmlbM0bo8Zp7wLb-LqsJZ9_e9SiHCPWu63R0cgXZSrrJw0E3skKRgRJmdIjrUXib4NzMtWv-1xhZTG6EgOxg6_Q87rA5vs0FE5bTDE4OEYyZagtL1G_spLn3bRjs_mA10cg1aDftU8I5SF2L5Rf8IW1bB7jy4XT4MuyBPJb8MYoo1uuEzsyMBwpKSKDuTaM3LdGbNzlh8VY8Bn2IDxlkTahGWDLSSG11FV_wzFhF8HmwchfCSJzaOxiY-L3zfe9Pa7mUK5PAdPCEKrXNrOHnw"
);

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
        assert!(
            String::from_utf8_lossy(&request[..length])
                .starts_with("GET /.well-known/jwks.json HTTP/1.1\r\n")
        );
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
    let mut verifier = WorkloadIdentityVerifier::new(WorkloadIdentityPolicy {
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
        .verify_authentication(
            &authentication(VALID_TOKEN.to_owned()),
            "POST",
            "/callback",
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
    let mut verifier = WorkloadIdentityVerifier::new(policy).expect("verifier creates");
    assert!(
        verifier
            .verify_authentication(
                &authentication(FOREIGN_TOKEN.to_owned()),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(1_000),
            )
            .is_err()
    );
    server.join().expect("server joins");

    assert_eq!(
        decode_header(VALID_TOKEN).expect("header decodes").alg,
        Algorithm::RS256
    );
    let (_, remainder) = VALID_TOKEN.split_once('.').expect("token header exists");
    let unsupported_header =
        URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT","kid":"key-1"}"#);
    let unsupported = format!("{unsupported_header}.{remainder}");
    assert!(
        verifier
            .verify_authentication(
                &authentication(unsupported),
                "POST",
                "/callback",
                UNIX_EPOCH + Duration::from_secs(1_000),
            )
            .is_err()
    );
}
