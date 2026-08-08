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
        let root = temporary_root();
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
        let server = start_server(&root, &data, &socket, port);

        let database = format!("telchar_{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
        let mut admin = connect(&socket, port, "postgres");
        admin
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .expect("test database creates");
        drop(admin);
        let url = format!(
            "postgresql://telchar@localhost/{database}?host={}&port={port}",
            percent_encode(socket.to_str().expect("UTF-8 socket directory"))
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
    pub fn restart(&mut self) {
        self.stop();
        self.server = Some(start_server(
            &self.root,
            &self.data,
            &self.socket,
            self.port,
        ));
    }

    #[allow(dead_code)]
    pub fn connect(&self) -> Client {
        Client::connect(&self.url, NoTls).expect("test database connects")
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

fn start_server(
    root: &std::path::Path,
    data: &std::path::Path,
    socket: &std::path::Path,
    port: u16,
) -> Child {
    let log_path = root.join("postgres.log");
    let log = fs::File::create(&log_path).expect("PostgreSQL log creates");
    let mut server = Command::new("postgres")
        .args([
            "-D",
            data.to_str().expect("UTF-8 data directory"),
            "-k",
            socket.to_str().expect("UTF-8 socket directory"),
            "-h",
            "",
            "-p",
            &port.to_string(),
            "-c",
            "fsync=off",
            "-c",
            "synchronous_commit=off",
            "-c",
            "full_page_writes=off",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("postgres starts");
    wait_until_ready(&mut server, socket, port, &log_path);
    server
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
