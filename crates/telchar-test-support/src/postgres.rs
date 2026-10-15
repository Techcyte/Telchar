// Provides shared integration-test helpers for PostgreSQL scenarios.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use postgres::{Client, NoTls};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct PostgresFixture {
    root: PathBuf,
    data: PathBuf,
    #[allow(dead_code)]
    socket: PathBuf,
    #[allow(dead_code)]
    port: u16,
    server: Option<Child>,
    url: String,
}

impl PostgresFixture {
    pub fn start() -> Self {
        Self::start_with_tls(None)
    }

    #[allow(dead_code)]
    pub fn start_tls() -> Self {
        let root = temporary_root();
        let certificate = create_tls_certificate(&root);
        Self::start_with_tls(Some(certificate))
    }

    fn start_with_tls(certificate: Option<TlsCertificate>) -> Self {
        let root = certificate
            .as_ref()
            .map_or_else(temporary_root, |certificate| certificate.root.clone());
        let data = root.join("data");
        let socket = root.join("socket");
        fs::create_dir_all(&socket).expect("PostgreSQL socket directory creates");
        Command::new("initdb")
            .args([
                "--auth=trust",
                "--encoding=UTF8",
                "--no-locale",
                "--username=telchar",
                "--pgdata",
                data.to_str().expect("UTF-8 data directory"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("initdb starts")
            .status
            .success()
            .then_some(())
            .expect("initdb succeeds");

        let port = available_port();
        let server = start_server(&root, &data, &socket, port, certificate.as_ref());

        let database = format!("telchar_{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
        let mut admin = connect(&socket, port, "postgres");
        admin
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .expect("test database creates");
        drop(admin);
        let url = certificate.as_ref().map_or_else(
            || {
                format!(
                    "postgresql://telchar@localhost/{database}?host={}&port={port}",
                    percent_encode(socket.to_str().expect("UTF-8 socket directory"))
                )
            },
            |certificate| {
                format!(
                    "postgresql://telchar@localhost:{port}/{database}?sslmode=verify-full&sslrootcert={}",
                    percent_encode(
                        certificate
                            .authority
                            .to_str()
                            .expect("UTF-8 CA certificate path")
                    )
                )
            },
        );
        Self {
            root,
            data,
            socket,
            port,
            server: Some(server),
            url,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    #[allow(dead_code)]
    pub fn keyword_url(&self) -> String {
        let database = database_name(&self.url);
        format!(
            "host={} port={} user=telchar dbname={database}",
            self.socket.to_str().expect("UTF-8 socket directory"),
            self.port
        )
    }

    #[allow(dead_code)]
    pub fn restart(&mut self) {
        self.stop();
        self.server = Some(start_server(
            &self.root,
            &self.data,
            &self.socket,
            self.port,
            None,
        ));
    }

    #[allow(dead_code)]
    pub fn connect(&self) -> Client {
        connect(&self.socket, self.port, database_name(&self.url))
    }

    #[allow(dead_code)]
    pub fn expire_singleton_ownership(&self, owner_kind: &str) {
        self.connect()
            .execute(
                "UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = $1",
                &[&owner_kind],
            )
            .expect("singleton ownership expires");
    }
}

impl Drop for PostgresFixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl PostgresFixture {
    fn stop(&mut self) {
        let _ = Command::new("pg_ctl")
            .args([
                "-D",
                self.data.to_str().unwrap_or_default(),
                "-m",
                "fast",
                "stop",
                "-w",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(mut server) = self.server.take() {
            let _ = server.wait();
        }
    }
}

struct TlsCertificate {
    root: PathBuf,
    authority: PathBuf,
    certificate: PathBuf,
    key: PathBuf,
}

fn create_tls_certificate(root: &std::path::Path) -> TlsCertificate {
    fs::create_dir_all(root).expect("TLS fixture directory creates");
    let authority = root.join("ca.crt");
    let authority_key = root.join("ca.key");
    let certificate = root.join("server.crt");
    let request = root.join("server.csr");
    let key = root.join("server.key");
    let extensions = root.join("server.ext");
    fs::write(
        &extensions,
        "subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
    )
    .expect("TLS certificate extensions write");
    Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=Telchar test CA",
            "-keyout",
            authority_key.to_str().expect("UTF-8 CA key path"),
            "-out",
            authority.to_str().expect("UTF-8 CA certificate path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("openssl starts")
        .status
        .success()
        .then_some(())
        .expect("TLS authority creates");
    Command::new("openssl")
        .args([
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-keyout",
            key.to_str().expect("UTF-8 key path"),
            "-out",
            request.to_str().expect("UTF-8 request path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("openssl starts")
        .status
        .success()
        .then_some(())
        .expect("TLS certificate request creates");
    Command::new("openssl")
        .args([
            "x509",
            "-req",
            "-days",
            "1",
            "-in",
            request.to_str().expect("UTF-8 request path"),
            "-CA",
            authority.to_str().expect("UTF-8 CA certificate path"),
            "-CAkey",
            authority_key.to_str().expect("UTF-8 CA key path"),
            "-CAcreateserial",
            "-extfile",
            extensions.to_str().expect("UTF-8 extension path"),
            "-out",
            certificate.to_str().expect("UTF-8 certificate path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("openssl starts")
        .status
        .success()
        .then_some(())
        .expect("TLS certificate creates");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600))
            .expect("TLS key permissions set");
    }
    TlsCertificate {
        root: root.to_path_buf(),
        authority,
        certificate,
        key,
    }
}

fn start_server(
    root: &std::path::Path,
    data: &std::path::Path,
    socket: &std::path::Path,
    port: u16,
    certificate: Option<&TlsCertificate>,
) -> Child {
    let log_path = root.join("postgres.log");
    let log = fs::File::create(&log_path).expect("PostgreSQL log creates");
    let mut command = Command::new("postgres");
    command.args([
        "-D",
        data.to_str().expect("UTF-8 data directory"),
        "-k",
        socket.to_str().expect("UTF-8 socket directory"),
        "-h",
        if certificate.is_some() {
            "127.0.0.1"
        } else {
            ""
        },
        "-p",
        &port.to_string(),
        "-c",
        "fsync=off",
        "-c",
        "synchronous_commit=off",
        "-c",
        "full_page_writes=off",
    ]);
    if let Some(certificate) = certificate {
        command.args([
            "-c",
            "ssl=on",
            "-c",
            &format!("ssl_cert_file={}", certificate.certificate.display()),
            "-c",
            &format!("ssl_key_file={}", certificate.key.display()),
        ]);
    }
    let mut server = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("postgres starts");
    wait_until_ready(&mut server, socket, port, &log_path);
    server
}

fn database_name(database_url: &str) -> &str {
    database_url
        .rsplit_once('/')
        .and_then(|(_, tail)| tail.split_once('?'))
        .map(|(database, _)| database)
        .expect("fixture database URL has database name")
}

fn connect(socket: &std::path::Path, port: u16, database: &str) -> Client {
    Client::connect(
        &format!(
            "host={} port={port} user=telchar dbname={database}",
            socket.to_str().expect("UTF-8 socket directory")
        ),
        NoTls,
    )
    .expect("PostgreSQL connects")
}

fn wait_until_ready(
    server: &mut Child,
    socket: &std::path::Path,
    port: u16,
    log_path: &std::path::Path,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Client::connect(
            &format!(
                "host={} port={port} user=telchar dbname=postgres connect_timeout=1",
                socket.to_str().expect("UTF-8 socket directory")
            ),
            NoTls,
        )
        .is_ok()
        {
            return;
        }
        if let Some(status) = server.try_wait().expect("PostgreSQL status reads") {
            let log = fs::read_to_string(log_path).unwrap_or_default();
            panic!("PostgreSQL exited before readiness with {status}: {log}");
        }
        if Instant::now() >= deadline {
            let _ = server.kill();
            let _ = server.wait();
            let log = fs::read_to_string(log_path).unwrap_or_default();
            panic!("PostgreSQL readiness deadline exceeded: {log}");
        }
        std::thread::yield_now();
    }
}

fn available_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("temporary port binds")
        .local_addr()
        .expect("temporary port address reads")
        .port()
}

fn temporary_root() -> PathBuf {
    PathBuf::from("/tmp").join(format!(
        "telchar-pg-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time follows epoch")
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
