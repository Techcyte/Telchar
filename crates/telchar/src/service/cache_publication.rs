//! Invokes an operator-controlled cache publication command after durable build success.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CachePublisher {
    executable: PathBuf,
    arguments: Vec<OsString>,
    timeout: Duration,
    maximum_input_bytes: usize,
}

impl CachePublisher {
    pub fn new(
        executable: impl AsRef<Path>,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
        timeout: Duration,
        maximum_input_bytes: usize,
    ) -> io::Result<Self> {
        let executable = executable.as_ref();
        if !executable.is_absolute() || timeout.is_zero() || maximum_input_bytes == 0 {
            return Err(invalid());
        }
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect::<Vec<_>>();
        if arguments.len() > 64
            || arguments
                .iter()
                .any(|argument| argument.as_encoded_bytes().len() > 4096)
        {
            return Err(invalid());
        }
        Ok(Self {
            executable: executable.to_path_buf(),
            arguments,
            timeout,
            maximum_input_bytes,
        })
    }

    pub fn publish(&self, outputs: &[String]) -> io::Result<()> {
        let input = serde_json::to_vec(outputs).map_err(|_| invalid())?;
        let input_length = input.len().checked_add(1).ok_or_else(invalid)?;
        if outputs.is_empty() || input_length > self.maximum_input_bytes {
            return Err(invalid());
        }
        let mut child = Command::new(&self.executable)
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| command_failed())?;
        let mut stdin = child.stdin.take().ok_or_else(command_failed)?;
        stdin.write_all(&input).map_err(|_| command_failed())?;
        stdin.write_all(b"\n").map_err(|_| command_failed())?;
        drop(stdin);

        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(status) = child.try_wait().map_err(|_| command_failed())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(command_failed())
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(command_failed());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "cache publication configuration is invalid",
    )
}

fn command_failed() -> io::Error {
    io::Error::other("cache publication command failed")
}
