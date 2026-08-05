use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const WORKER_MAGIC_1: u64 = 0x6e69_7863;
const WORKER_MAGIC_2: u64 = 0x6478_696f;
const CLIENT_METADATA_BYTES: usize = 48;
const PEER_METADATA_BYTES: usize = 16;

pub struct TraceCapture {
    socket_path: PathBuf,
    client_metadata: Arc<Mutex<Vec<u8>>>,
    peer_metadata: Arc<Mutex<Vec<u8>>>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

pub struct WorkerTrace {
    client_protocol_version: (u8, u8),
    peer_protocol_version: (u8, u8),
    operations: Vec<u64>,
}

impl TraceCapture {
    pub fn start(peer_socket: &str) -> io::Result<Self> {
        let socket_path = std::env::temp_dir().join(format!(
            "telchar-worker-trace-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let listener = UnixListener::bind(&socket_path)?;
        let client_metadata = Arc::new(Mutex::new(Vec::new()));
        let peer_metadata = Arc::new(Mutex::new(Vec::new()));
        let worker_client_metadata = Arc::clone(&client_metadata);
        let worker_peer_metadata = Arc::clone(&peer_metadata);
        let peer_socket = peer_socket.to_owned();

        let worker = thread::spawn(move || {
            let (client, _) = listener.accept()?;
            let peer = UnixStream::connect(peer_socket)?;
            let client_reader = client.try_clone()?;
            let peer_writer = peer.try_clone()?;
            let client_metadata_worker = thread::spawn(move || {
                relay(
                    client_reader,
                    peer_writer,
                    worker_client_metadata,
                    CLIENT_METADATA_BYTES,
                )
            });
            let peer_result = relay(peer, client, worker_peer_metadata, PEER_METADATA_BYTES);
            let client_result = client_metadata_worker
                .join()
                .map_err(|_| io::Error::other("client relay panicked"))?;
            peer_result?;
            client_result
        });

        tracing::info!(
            event = "worker.trace.capture.started",
            connection = "local-unix",
            "Worker trace capture started"
        );
        Ok(Self {
            socket_path,
            client_metadata,
            peer_metadata,
            worker: Some(worker),
        })
    }

    pub fn store_url(&self) -> String {
        format!("unix://{}", self.socket_path.display())
    }

    pub fn finish(mut self) -> io::Result<WorkerTrace> {
        let worker = self
            .worker
            .take()
            .expect("worker trace capture finishes once");
        worker
            .join()
            .map_err(|_| io::Error::other("worker trace capture panicked"))??;
        let _ = std::fs::remove_file(&self.socket_path);

        let client = self.client_metadata.lock().expect("client metadata");
        let peer = self.peer_metadata.lock().expect("peer metadata");
        let client_magic = read_word(&client, 0)?;
        let peer_magic = read_word(&peer, 0)?;
        if client_magic != WORKER_MAGIC_1 || peer_magic != WORKER_MAGIC_2 {
            return Err(io::Error::other("worker trace handshake magic mismatch"));
        }
        let operation = read_word(&client, 40)?;
        let trace = WorkerTrace {
            client_protocol_version: protocol_version(read_word(&client, 8)?),
            peer_protocol_version: protocol_version(read_word(&peer, 8)?),
            operations: vec![operation],
        };
        tracing::info!(
            event = "worker.trace.capture.finished",
            protocol_major = trace.client_protocol_version.0,
            protocol_minor = trace.client_protocol_version.1,
            operation_count = trace.operations.len(),
            "Worker trace capture finished"
        );
        Ok(trace)
    }
}

impl WorkerTrace {
    pub fn client_protocol_version(&self) -> (u8, u8) {
        self.client_protocol_version
    }

    pub fn peer_protocol_version(&self) -> (u8, u8) {
        self.peer_protocol_version
    }

    pub fn operations(&self) -> &[u64] {
        &self.operations
    }

    pub fn contains_payloads(&self) -> bool {
        false
    }

    pub fn sanitized_json(&self) -> String {
        format!(
            "{{\"client_protocol\":\"{}.{}\",\"operations\":{:?},\"peer_protocol\":\"{}.{}\"}}",
            self.client_protocol_version.0,
            self.client_protocol_version.1,
            self.operations,
            self.peer_protocol_version.0,
            self.peer_protocol_version.1
        )
    }
}

fn relay(
    mut source: UnixStream,
    mut destination: UnixStream,
    metadata: Arc<Mutex<Vec<u8>>>,
    limit: usize,
) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            destination.shutdown(std::net::Shutdown::Write)?;
            return Ok(());
        }
        let mut metadata = metadata.lock().expect("trace metadata");
        let remaining = limit.saturating_sub(metadata.len());
        metadata.extend_from_slice(&buffer[..count.min(remaining)]);
        drop(metadata);
        destination.write_all(&buffer[..count])?;
    }
}

fn read_word(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let word = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| io::Error::other("worker trace metadata incomplete"))?;
    Ok(u64::from_le_bytes(word.try_into().expect("word length")))
}

fn protocol_version(wire: u64) -> (u8, u8) {
    ((wire >> 8) as u8, wire as u8)
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
