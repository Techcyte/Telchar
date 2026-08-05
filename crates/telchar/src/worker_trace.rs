use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, FEATURE_NEGOTIATION_VERSION, SERVER_WORKER_MAGIC, STDERR_LAST,
    WorkerOperation, WorkerVersion,
};

const MAXIMUM_HANDSHAKE_FEATURES: u64 = 64;
const MAXIMUM_HANDSHAKE_FEATURE_LENGTH: u64 = 1024;
const MAXIMUM_OPTION_OVERRIDES: u64 = 256;
const MAXIMUM_OPTION_STRING_LENGTH: u64 = 16_384;

pub struct TraceCapture {
    socket_path: PathBuf,
    trace: Arc<Mutex<WorkerTrace>>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

pub const TRACE_RELAY_BUFFER_BYTES: usize = 4096;

#[derive(Debug, Default)]
pub struct WorkerTrace {
    client_protocol_version: Option<WorkerVersion>,
    peer_protocol_version: Option<WorkerVersion>,
    operations: Vec<WorkerOperation>,
    feature_lengths: Vec<u64>,
    daemon_version_length: Option<u64>,
    override_count: Option<u64>,
    option_string_lengths: Vec<u64>,
    terminal_frames: usize,
}

impl TraceCapture {
    pub fn start(peer_socket: &str) -> io::Result<Self> {
        let socket_path = std::env::temp_dir().join(format!(
            "telchar-worker-trace-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let listener = UnixListener::bind(&socket_path)?;
        let trace = Arc::new(Mutex::new(WorkerTrace::default()));
        let worker_trace = Arc::clone(&trace);
        let peer_socket = peer_socket.to_owned();

        let worker = thread::spawn(move || {
            let (client, _) = listener.accept()?;
            let peer = UnixStream::connect(peer_socket)?;
            relay_fixture_flow(client, peer, worker_trace)
        });

        tracing::info!(
            event = "worker.trace.capture.started",
            connection = "local-unix",
            "Worker trace capture started"
        );
        Ok(Self {
            socket_path,
            trace,
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
        let trace = std::mem::take(&mut *self.trace.lock().expect("worker trace"));
        tracing::info!(
            event = "worker.trace.capture.finished",
            operation_count = trace.operations.len(),
            terminal_frame_count = trace.terminal_frames,
            "Worker trace capture finished"
        );
        Ok(trace)
    }
}

impl WorkerTrace {
    pub fn client_protocol_version(&self) -> (u8, u8) {
        version_parts(
            self.client_protocol_version
                .expect("client protocol version"),
        )
    }

    pub fn peer_protocol_version(&self) -> (u8, u8) {
        version_parts(self.peer_protocol_version.expect("peer protocol version"))
    }

    pub fn operations(&self) -> &[WorkerOperation] {
        &self.operations
    }

    pub fn contains_payloads(&self) -> bool {
        false
    }

    pub fn sanitized_json(&self) -> String {
        format!(
            "{{\"client_protocol\":\"{}.{}\",\"operations\":{:?},\"peer_protocol\":\"{}.{}\"}}",
            self.client_protocol_version().0,
            self.client_protocol_version().1,
            self.operations,
            self.peer_protocol_version().0,
            self.peer_protocol_version().1
        )
    }
}

fn relay_fixture_flow(
    mut client: UnixStream,
    mut peer: UnixStream,
    trace: Arc<Mutex<WorkerTrace>>,
) -> io::Result<()> {
    let client_version = relay_handshakes(&mut client, &mut peer, &trace)?;
    let version = client_version.min(
        trace
            .lock()
            .expect("worker trace")
            .peer_protocol_version
            .expect("peer protocol version"),
    );

    relay_client_post_handshake(&mut client, &mut peer, version)?;
    relay_peer_handshake_info(&mut peer, &mut client, version, &trace)?;
    relay_terminal_frame(&mut peer, &mut client, &trace)?;
    relay_set_options(&mut client, &mut peer, &trace)?;
    relay_terminal_frame(&mut peer, &mut client, &trace)?;
    Ok(())
}

fn relay_handshakes(
    client: &mut (impl Read + Write),
    peer: &mut (impl Read + Write),
    trace: &Arc<Mutex<WorkerTrace>>,
) -> io::Result<WorkerVersion> {
    if relay_word(client, peer)? != CLIENT_WORKER_MAGIC {
        return Err(invalid("worker client handshake magic mismatch"));
    }
    if relay_word(peer, client)? != SERVER_WORKER_MAGIC {
        return Err(invalid("worker server handshake magic mismatch"));
    }
    let peer_version = WorkerVersion::from_wire(relay_word(peer, client)?);
    let client_version = WorkerVersion::from_wire(relay_word(client, peer)?);
    let client_feature_lengths = if client_version >= FEATURE_NEGOTIATION_VERSION {
        relay_strings(
            client,
            peer,
            MAXIMUM_HANDSHAKE_FEATURES,
            MAXIMUM_HANDSHAKE_FEATURE_LENGTH,
        )?
    } else {
        Vec::new()
    };
    let peer_feature_lengths = if client_version.min(peer_version) >= FEATURE_NEGOTIATION_VERSION {
        relay_strings(
            peer,
            client,
            MAXIMUM_HANDSHAKE_FEATURES,
            MAXIMUM_HANDSHAKE_FEATURE_LENGTH,
        )?
    } else {
        Vec::new()
    };
    let mut trace = trace.lock().expect("worker trace");
    trace.client_protocol_version = Some(client_version);
    trace.peer_protocol_version = Some(peer_version);
    trace.feature_lengths.extend(client_feature_lengths);
    trace.feature_lengths.extend(peer_feature_lengths);
    Ok(client_version)
}

fn relay_client_post_handshake(
    source: &mut impl Read,
    destination: &mut impl Write,
    version: WorkerVersion,
) -> io::Result<()> {
    if version >= WorkerVersion::new(1, 14) && relay_word(source, destination)? != 0 {
        relay_word(source, destination)?;
    }
    if version >= WorkerVersion::new(1, 11) {
        relay_word(source, destination)?;
    }
    Ok(())
}

fn relay_peer_handshake_info(
    source: &mut impl Read,
    destination: &mut impl Write,
    version: WorkerVersion,
    trace: &Arc<Mutex<WorkerTrace>>,
) -> io::Result<()> {
    if version >= WorkerVersion::new(1, 33) {
        let length = relay_string(source, destination, MAXIMUM_HANDSHAKE_FEATURE_LENGTH)?;
        trace.lock().expect("worker trace").daemon_version_length = Some(length);
    }
    if version >= WorkerVersion::new(1, 35) {
        let trust_status = relay_word(source, destination)?;
        if trust_status > 2 {
            return Err(invalid("worker trust status is invalid"));
        }
    }
    Ok(())
}

fn relay_set_options(
    source: &mut impl Read,
    destination: &mut impl Write,
    trace: &Arc<Mutex<WorkerTrace>>,
) -> io::Result<()> {
    if relay_word(source, destination)? != 19 {
        return Err(invalid("untyped worker operation"));
    }
    for _ in 0..12 {
        relay_word(source, destination)?;
    }
    let override_count = relay_word(source, destination)?;
    if override_count > MAXIMUM_OPTION_OVERRIDES {
        return Err(invalid("worker option override count exceeds limit"));
    }
    let mut string_lengths = Vec::with_capacity((override_count * 2) as usize);
    for _ in 0..override_count {
        string_lengths.push(relay_string(
            source,
            destination,
            MAXIMUM_OPTION_STRING_LENGTH,
        )?);
        string_lengths.push(relay_string(
            source,
            destination,
            MAXIMUM_OPTION_STRING_LENGTH,
        )?);
    }
    let mut trace = trace.lock().expect("worker trace");
    trace.operations.push(WorkerOperation::SetOptions);
    trace.override_count = Some(override_count);
    trace.option_string_lengths = string_lengths;
    Ok(())
}

fn relay_terminal_frame(
    source: &mut impl Read,
    destination: &mut impl Write,
    trace: &Arc<Mutex<WorkerTrace>>,
) -> io::Result<()> {
    if relay_word(source, destination)? != STDERR_LAST {
        return Err(invalid("untyped worker response, callback, or upload"));
    }
    trace.lock().expect("worker trace").terminal_frames += 1;
    Ok(())
}

fn relay_strings(
    source: &mut impl Read,
    destination: &mut impl Write,
    maximum_count: u64,
    maximum_length: u64,
) -> io::Result<Vec<u64>> {
    let count = relay_word(source, destination)?;
    if count > maximum_count {
        return Err(invalid("worker string count exceeds limit"));
    }
    (0..count)
        .map(|_| relay_string(source, destination, maximum_length))
        .collect()
}

fn relay_string(
    source: &mut impl Read,
    destination: &mut impl Write,
    maximum_length: u64,
) -> io::Result<u64> {
    let length = relay_word(source, destination)?;
    if length > maximum_length {
        return Err(invalid("worker string exceeds limit"));
    }
    relay_exact(
        source,
        destination,
        usize::try_from(length).map_err(|_| invalid("worker string exceeds limit"))?,
    )?;
    let padding = (8 - length % 8) % 8;
    let mut padding_bytes = [0; 7];
    source.read_exact(&mut padding_bytes[..padding as usize])?;
    if padding_bytes[..padding as usize]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(invalid("worker string padding is not zero"));
    }
    destination.write_all(&padding_bytes[..padding as usize])?;
    Ok(length)
}

fn relay_word(source: &mut impl Read, destination: &mut impl Write) -> io::Result<u64> {
    let mut bytes = [0; 8];
    source.read_exact(&mut bytes)?;
    destination.write_all(&bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn relay_exact(
    source: &mut impl Read,
    destination: &mut impl Write,
    mut remaining: usize,
) -> io::Result<()> {
    let mut buffer = [0; TRACE_RELAY_BUFFER_BYTES];
    while remaining > 0 {
        let length = remaining.min(buffer.len());
        source.read_exact(&mut buffer[..length])?;
        destination.write_all(&buffer[..length])?;
        remaining -= length;
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn version_parts(version: WorkerVersion) -> (u8, u8) {
    let wire = version.to_wire();
    ((wire >> 8) as u8, wire as u8)
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
