use std::io::{self, Write};
use std::os::unix::net::UnixStream;

pub(super) struct SessionOutput {
    stream: UnixStream,
    pending: Vec<u8>,
    offset: usize,
    truncated: bool,
}

impl SessionOutput {
    pub(super) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            pending: Vec::new(),
            offset: 0,
            truncated: false,
        }
    }

    pub(super) fn log(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.drain_available()?;
        if self.offset != self.pending.len() {
            self.truncated = true;
            return Ok(());
        }
        self.pending.clear();
        self.offset = 0;
        if self.truncated {
            nix_worker_protocol::write_stderr_frame(
                &mut self.pending,
                nix_worker_protocol::StderrFrame::Next {
                    message: crate::shared_build::LIVE_LOG_TRUNCATION_MARKER.to_vec(),
                },
            )?;
            self.truncated = false;
        }
        let maximum = nix_worker_protocol::MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES;
        nix_worker_protocol::write_stderr_frame(
            &mut self.pending,
            nix_worker_protocol::StderrFrame::Next {
                message: chunk[..chunk.len().min(maximum)].to_vec(),
            },
        )?;
        self.truncated = chunk.len() > maximum;
        self.drain_available()
    }

    fn drain_available(&mut self) -> io::Result<()> {
        while self.offset < self.pending.len() {
            match rustix::net::send(
                &self.stream,
                &self.pending[self.offset..],
                rustix::net::SendFlags::DONTWAIT | rustix::net::SendFlags::NOSIGNAL,
            ) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => self.offset += count,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn finish_logs(&mut self) -> io::Result<()> {
        self.stream.write_all(&self.pending[self.offset..])?;
        self.pending.clear();
        self.offset = 0;
        if self.truncated {
            nix_worker_protocol::write_stderr_frame(
                &mut self.stream,
                nix_worker_protocol::StderrFrame::Next {
                    message: crate::shared_build::LIVE_LOG_TRUNCATION_MARKER.to_vec(),
                },
            )?;
            self.truncated = false;
        }
        Ok(())
    }
}

impl Write for SessionOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.finish_logs()?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.finish_logs()?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn saturated_logs_preserve_frames_and_bound_pending_bytes() {
        let (stream, mut reader) = UnixStream::pair().unwrap();
        let mut output = SessionOutput::new(stream);
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..4096 {
            output.log(&chunk).unwrap();
            assert!(
                output.pending.len()
                    <= nix_worker_protocol::MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES + 128
            );
        }
        assert!(output.truncated);
        assert!(output.offset < output.pending.len());
        let receiving = std::thread::spawn(move || {
            let mut wire = Vec::new();
            reader.read_to_end(&mut wire).unwrap();
            wire
        });
        output
            .write_all(&nix_worker_protocol::STDERR_LAST.to_le_bytes())
            .unwrap();
        drop(output);
        let wire = receiving.join().unwrap();
        let mut remaining = wire.as_slice();
        let mut truncated = false;
        loop {
            let mut integer = [0; 8];
            remaining.read_exact(&mut integer).unwrap();
            let frame = u64::from_le_bytes(integer);
            if frame == nix_worker_protocol::STDERR_LAST {
                break;
            }
            assert_eq!(frame, nix_worker_protocol::STDERR_NEXT);
            remaining.read_exact(&mut integer).unwrap();
            let length = u64::from_le_bytes(integer) as usize;
            let body = &remaining[..length];
            truncated |= body == crate::shared_build::LIVE_LOG_TRUNCATION_MARKER;
            assert!(
                body == &chunk[..nix_worker_protocol::MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES]
                    || body == crate::shared_build::LIVE_LOG_TRUNCATION_MARKER
            );
            remaining = &remaining[length + (8 - length % 8) % 8..];
        }
        assert!(truncated);
        assert!(remaining.is_empty());
    }
}
