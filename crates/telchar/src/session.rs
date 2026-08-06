use std::io::{self, Write};
use std::time::Duration;

use nix_worker_protocol::{ProtocolSessionLimits, WorkerInput, WorkerOperation, WorkerReader};

pub struct SessionInput {
    input: std::os::unix::net::UnixStream,
    idle_timeout: Duration,
    deadline: Option<std::time::Instant>,
}

impl SessionInput {
    pub fn new(input: std::os::unix::net::UnixStream, idle_timeout: Duration) -> Self {
        Self {
            input,
            idle_timeout,
            deadline: None,
        }
    }
}

impl io::Read for SessionInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        self.input.set_read_timeout(timeout)?;
        let received = self.input.read(buffer).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) {
                io::Error::new(io::ErrorKind::TimedOut, "worker protocol input timed out")
            } else {
                error
            }
        })?;
        if received > 0 {
            self.deadline = Some(std::time::Instant::now() + self.idle_timeout);
        }
        Ok(received)
    }
}

impl WorkerInput for SessionInput {
    fn complete_message(&mut self) {
        self.deadline = None;
        let _ = self.input.set_read_timeout(None);
    }
}

pub fn run_worker_session(
    input: std::os::unix::net::UnixStream,
    mut output: std::os::unix::net::UnixStream,
    limits: ProtocolSessionLimits,
) -> io::Result<()> {
    let input = SessionInput::new(input, limits.incomplete_message_idle_timeout);
    let mut reader = WorkerReader::new(input, limits);
    let negotiated = reader.perform_server_handshake(&mut output, &[])?;
    reader.complete_server_post_handshake(&mut output, negotiated.version, "telchar")?;

    loop {
        match reader.read_operation() {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(error),
            Err(_) => return reject(&mut output, "unknown worker operation"),
            Ok(WorkerOperation::SetOptions) => {
                if reader.complete_set_options().is_err() {
                    return reject(&mut output, "invalid SetOptions request");
                }
                output.write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())?;
                output.flush()?;
                tracing::info!(
                    event = "worker.set_options.completed",
                    "SetOptions request completed"
                );
            }
            Ok(operation) if !operation.is_fixture_allowed() => {
                return reject(&mut output, "unsupported worker operation");
            }
            Ok(_) => return reject(&mut output, "unsupported worker operation"),
        }
    }
}

fn reject(output: &mut impl Write, message: &str) -> io::Result<()> {
    nix_worker_protocol::write_worker_error(output, message)?;
    output.flush()
}
