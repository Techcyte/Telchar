use std::io::{self, Read};
use std::os::fd::AsFd;
#[cfg(test)]
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use rustix::event::{poll, PollFd, PollFlags, Timespec};

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
        let started = Instant::now();
        let deadline = self
            .deadline
            .get_or_insert_with(|| started + self.idle_timeout);
        let remaining = deadline.saturating_duration_since(started);
        let descriptor = PollFd::new(&self.input, PollFlags::IN);
        let timeout = timeout_as_timespec(remaining)?;
        if poll(&mut [descriptor], Some(&timeout))? == 0 {
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
    use std::time::{Duration, Instant};

    use super::{StdioInput, TestInput};
    use nix_worker_protocol::WorkerInput;

    #[test]
    fn incomplete_input_times_out_without_leaking_the_reader() {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let mut input = StdioInput::new(TestInput::new(server), Duration::from_millis(20));
        let started = Instant::now();
        let mut byte = [0; 1];

        let error = input.read(&mut byte).expect_err("idle input times out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(input);
        client
            .shutdown(std::net::Shutdown::Both)
            .expect("client closes");
    }

    #[test]
    fn input_progress_resets_the_idle_deadline() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let mut input = StdioInput::new(TestInput::new(server), Duration::from_millis(40));
        client.write_all(&[1]).expect("first byte writes");
        let mut byte = [0; 1];
        input.read_exact(&mut byte).expect("first byte reads");
        std::thread::sleep(Duration::from_millis(25));
        client.write_all(&[2]).expect("second byte writes");
        input.read_exact(&mut byte).expect("second byte reads");
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
}
