use std::io::{self, Read};
use std::os::fd::AsFd;
#[cfg(test)]
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};

pub struct StdioInput<R> {
    input: R,
    idle_timeout: Duration,
    deadline: Option<Instant>,
}

impl<R> StdioInput<R> {
    pub fn new(input: R, idle_timeout: Duration) -> Self {
        Self {
            input,
            idle_timeout,
            deadline: None,
        }
    }
}

impl<R: Read + AsFd> Read for StdioInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self
            .deadline
            .map(|deadline| timeout_as_timespec(deadline.saturating_duration_since(Instant::now())))
            .transpose()?;
        let descriptor = PollFd::new(&self.input, PollFlags::IN);
        if poll(&mut [descriptor], timeout.as_ref())? == 0 {
            tracing::error!(
                event = "worker.session.timed_out",
                timeout_ms = self.idle_timeout.as_millis() as u64,
                "worker protocol session timed out"
            );
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "worker protocol input timed out",
            ));
        }
        let received = self.input.read(buffer)?;
        if received > 0 {
            self.deadline = Some(Instant::now() + self.idle_timeout);
        }
        Ok(received)
    }
}

impl<R> nix_worker_protocol::WorkerInput for StdioInput<R>
where
    R: Read + AsFd,
{
    fn complete_message(&mut self) {
        self.deadline = None;
    }
}

fn timeout_as_timespec(timeout: Duration) -> io::Result<Timespec> {
    let seconds = i64::try_from(timeout.as_secs())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "idle timeout is too large"))?;
    Ok(Timespec {
        tv_sec: seconds,
        tv_nsec: timeout.subsec_nanos().into(),
    })
}

#[cfg(test)]
pub struct TestInput {
    reader: std::os::unix::net::UnixStream,
}

#[cfg(test)]
impl TestInput {
    pub fn new(reader: std::os::unix::net::UnixStream) -> Self {
        Self { reader }
    }
}

#[cfg(test)]
impl Read for TestInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buffer)
    }
}

#[cfg(test)]
impl AsFd for TestInput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.reader.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{StdioInput, TestInput};
    use nix_worker_protocol::{
        CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, ProtocolSessionLimits, WorkerInput,
        WorkerReader,
    };
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    #[test]
    fn incomplete_input_times_out_without_leaking_the_reader() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let mut input = StdioInput::new(TestInput::new(server), Duration::from_millis(20));
        let mut byte = [0; 1];
        client.write_all(&[1]).expect("partial message writes");
        input.read_exact(&mut byte).expect("partial message reads");

        let timed_out = Arc::new(AtomicBool::new(false));
        let subscriber = tracing_subscriber::registry().with(TimeoutEvents(Arc::clone(&timed_out)));
        let error = tracing::subscriber::with_default(subscriber, || {
            input
                .read(&mut byte)
                .expect_err("partial message times out")
        });

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            timed_out.load(Ordering::Relaxed),
            "timeout telemetry is emitted"
        );
        drop(input);
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("client read timeout sets");
        assert_eq!(client.read(&mut byte).expect("server descriptor closes"), 0);
    }

    struct TimeoutEvents(Arc<AtomicBool>);

    impl<S> Layer<S> for TimeoutEvents
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut fields = EventFields::default();
            event.record(&mut fields);
            if fields.event.as_deref() == Some("worker.session.timed_out") {
                self.0.store(true, Ordering::Relaxed);
            }
        }
    }

    #[derive(Default)]
    struct EventFields {
        event: Option<String>,
    }

    impl Visit for EventFields {
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "event" {
                self.event = Some(value.to_owned());
            }
        }

        fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    }

    #[test]
    fn input_progress_resets_the_idle_deadline() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let timeout = Duration::from_millis(40);
        let mut input = StdioInput::new(TestInput::new(server), timeout);
        let started = Instant::now();
        client.write_all(&[1]).expect("first byte writes");
        let mut byte = [0; 1];
        input.read_exact(&mut byte).expect("first byte reads");
        thread::sleep(Duration::from_millis(25));
        client.write_all(&[2]).expect("second byte writes");
        input.read_exact(&mut byte).expect("second byte reads");
        thread::sleep(Duration::from_millis(25));
        client.write_all(&[3]).expect("third byte writes");
        input.read_exact(&mut byte).expect("third byte reads");
        assert!(
            started.elapsed() > timeout,
            "progress extends the original deadline"
        );
    }

    #[test]
    fn complete_handshake_can_wait_without_operation_input() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let timeout = Duration::from_millis(20);
        write_worker_integer(&mut client, CLIENT_WORKER_MAGIC);
        write_worker_integer(&mut client, LATEST_WORKER_VERSION.to_wire());
        write_worker_integer(&mut client, 0);
        write_worker_integer(&mut client, 0);
        write_worker_integer(&mut client, 0);
        client.flush().expect("handshake writes flush");

        let (result_sender, result_receiver) = mpsc::channel();
        thread::spawn(move || {
            let input = StdioInput::new(TestInput::new(server), timeout);
            let mut reader = WorkerReader::new(input, ProtocolSessionLimits::new(1024, timeout));
            let mut output = Vec::new();
            let result = reader
                .perform_server_handshake(&mut output, &[])
                .and_then(|negotiated| {
                    reader.complete_server_post_handshake(
                        &mut output,
                        negotiated.version,
                        "telchar",
                    )
                })
                .and_then(|_| reader.read_operation());
            result_sender.send(result).expect("operation result sends");
        });

        assert!(
            result_receiver.recv_timeout(timeout * 2).is_err(),
            "complete handshake idle session remains alive"
        );
        write_worker_integer(&mut client, 1);
        client.flush().expect("operation flushes");
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("operation result returns")
                .expect("operation read"),
            nix_worker_protocol::WorkerOperation::IsValidPath
        );
    }

    #[test]
    fn complete_message_boundary_does_not_expire() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let mut input = StdioInput::new(TestInput::new(server), Duration::from_millis(20));
        client.write_all(&[1]).expect("message byte writes");
        let mut byte = [0; 1];
        input.read_exact(&mut byte).expect("message byte reads");
        input.complete_message();
        std::thread::sleep(Duration::from_millis(40));
        client.write_all(&[2]).expect("next message byte writes");
        input
            .read_exact(&mut byte)
            .expect("next message byte reads");
    }

    fn write_worker_integer(output: &mut impl Write, value: u64) {
        output
            .write_all(&value.to_le_bytes())
            .expect("worker integer writes");
    }
}
