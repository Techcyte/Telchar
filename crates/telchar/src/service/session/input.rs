use super::*;

pub(super) struct SessionInput {
    input: std::os::unix::net::UnixStream,
    idle_timeout: Duration,
    deadline: Option<std::time::Instant>,
}

impl SessionInput {
    pub(super) fn new(input: std::os::unix::net::UnixStream, idle_timeout: Duration) -> Self {
        Self {
            input,
            idle_timeout,
            deadline: None,
        }
    }
}

pub(super) fn requester_disconnected(
    stream: &mut std::os::unix::net::UnixStream,
) -> io::Result<bool> {
    let mut descriptor = [rustix::event::PollFd::new(
        &*stream,
        rustix::event::PollFlags::IN | rustix::event::PollFlags::HUP,
    )];
    let timeout = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    rustix::event::poll(&mut descriptor, Some(&timeout))?;
    let events = descriptor[0].revents();
    if events.contains(rustix::event::PollFlags::HUP) {
        return Ok(true);
    }
    if events.contains(rustix::event::PollFlags::IN) {
        let mut byte = [std::mem::MaybeUninit::uninit(); 1];
        match rustix::net::recv(
            &*stream,
            &mut byte,
            rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT,
        ) {
            Ok((_, 0)) => return Ok(true),
            Ok(_) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => {}
            Err(error) => return Err(io::Error::from(error)),
        }
    }
    Ok(false)
}

impl io::Read for SessionInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        if timeout == Some(Duration::ZERO) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "worker protocol input timed out",
            ));
        }
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

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    #[test]
    fn expired_partial_message_returns_timeout() {
        let (input, mut writer) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        writer.write_all(b"a").expect("initial byte");

        let mut input = SessionInput::new(input, Duration::from_millis(1));
        let mut byte = [0; 1];
        assert_eq!(input.read(&mut byte).expect("initial read"), 1);
        std::thread::sleep(Duration::from_millis(10));

        let error = input
            .read(&mut byte)
            .expect_err("expired read must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "worker protocol input timed out");
    }
}
