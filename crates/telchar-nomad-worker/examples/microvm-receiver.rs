//! Disposable execution-authenticated WSS receiver fixture.

use rustls::pki_types::pem::PemObject;

struct Authentication {
    execution: String,
    instance: String,
    credential: String,
    consumed: bool,
}

impl Authentication {
    fn new(execution: &str, instance: &str, credential: &str) -> Self {
        Self {
            execution: execution.to_owned(),
            instance: instance.to_owned(),
            credential: format!("Bearer {credential}"),
            consumed: false,
        }
    }

    fn accept(&mut self, request: &tungstenite::http::Request<()>) -> bool {
        let matches = |name: &str, expected: &str| {
            request.headers().get(name).is_some_and(|value| value.as_bytes() == expected.as_bytes())
        };
        if self.consumed || request.uri().path() != "/fixture"
            || !matches("x-telchar-execution", &self.execution)
            || !matches("x-telchar-microvm", &self.instance)
            || !matches("authorization", &self.credential)
        {
            return false;
        }
        self.consumed = true;
        true
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err("expected config.json certificate.pem key.pem listen-address".into());
    }
    let configuration: serde_json::Value = serde_json::from_reader(std::fs::File::open(&arguments[0])?)?;
    let required = |name: &str| configuration[name].as_str().ok_or("missing execution field");
    let mut authentication = Authentication::new(
        required("execution")?, required("microvmId")?, required("credential")?,
    );
    let certificates = rustls::pki_types::CertificateDer::pem_file_iter(&arguments[1])?
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&arguments[2])?;
    let tls = std::sync::Arc::new(rustls::ServerConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth().with_single_cert(certificates, key)?);
    let listener = std::net::TcpListener::bind(&arguments[3])?;
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while std::time::Instant::now() < deadline {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let stream = rustls::StreamOwned::new(rustls::ServerConnection::new(tls.clone())?, stream);
        let accepted = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, response| {
            if authentication.accept(request) {
                Ok(response)
            } else {
                Err(tungstenite::http::Response::builder().status(403).body(None).expect("valid rejection"))
            }
        });
        match accepted {
            Ok(mut socket) => {
                socket.close(None)?;
                println!("execution authentication accepted");
                return Ok(());
            }
            Err(_) => eprintln!("execution authentication rejected"),
        }
    }
    Err("receiver lifetime exhausted".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::http::Request;

    #[test]
    fn authentication_binds_instance_execution_and_single_use_credential() {
        let mut authentication = Authentication::new("execution", "instance", "secret");
        let request = |execution: &str, instance: &str, credential: &str| {
            Request::builder()
                .uri("/fixture")
                .header("x-telchar-execution", execution)
                .header("x-telchar-microvm", instance)
                .header("authorization", format!("Bearer {credential}"))
                .body(())
                .unwrap()
        };
        for rejected in [
            request("foreign", "instance", "secret"),
            request("execution", "foreign", "secret"),
            request("execution", "instance", "wrong"),
        ] {
            assert!(!authentication.accept(&rejected));
        }
        let accepted = request("execution", "instance", "secret");
        assert!(authentication.accept(&accepted));
        assert!(!authentication.accept(&accepted));
    }
}
