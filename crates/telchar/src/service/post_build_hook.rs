//! Invokes an operator-controlled post-build hook command after durable build success.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostBuildHook {
    executable: PathBuf,
    arguments: Vec<OsString>,
    timeout: Duration,
    maximum_input_bytes: usize,
}

impl PostBuildHook {
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

    pub fn run(&self, outputs: &[String]) -> io::Result<()> {
        let input = serde_json::to_vec(outputs).map_err(|_| invalid())?;
        let input_length = input.len().checked_add(1).ok_or_else(invalid)?;
        if outputs.is_empty() || input_length > self.maximum_input_bytes {
            return Err(invalid());
        }
        let mut child = Command::new(&self.executable)
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| command_failed())?;
        let mut stdout = child.stdout.take().ok_or_else(command_failed)?;
        let mut stderr = child.stderr.take().ok_or_else(command_failed)?;
        rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| command_failed())?;
        rustix::fs::fcntl_setfl(&stderr, rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| command_failed())?;
        let mut diagnostics = [Vec::new(), Vec::new()];
        let mut truncated = [false; 2];
        let mut stdin = child.stdin.take();
        rustix::fs::fcntl_setfl(
            stdin.as_ref().ok_or_else(command_failed)?,
            rustix::fs::OFlags::NONBLOCK,
        )
        .map_err(|_| command_failed())?;
        let mut input = input;
        input.push(b'\n');
        let mut written = 0;
        let deadline = Instant::now() + self.timeout;
        loop {
            collect_output(&mut stdout, &mut diagnostics[0], &mut truncated[0]);
            collect_output(&mut stderr, &mut diagnostics[1], &mut truncated[1]);
            if let Some(pipe) = stdin.as_mut() {
                match pipe.write(&input[written..]) {
                    Ok(count) => {
                        written += count;
                        if written == input.len() {
                            stdin = None;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        log_output(&diagnostics, &truncated);
                        return Err(command_failed());
                    }
                }
            }
            if let Some(status) = child.try_wait().map_err(|_| command_failed())? {
                collect_output(&mut stdout, &mut diagnostics[0], &mut truncated[0]);
                collect_output(&mut stderr, &mut diagnostics[1], &mut truncated[1]);
                log_output(&diagnostics, &truncated);
                return if status.success() {
                    Ok(())
                } else {
                    Err(command_failed())
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                collect_output(&mut stdout, &mut diagnostics[0], &mut truncated[0]);
                collect_output(&mut stderr, &mut diagnostics[1], &mut truncated[1]);
                log_output(&diagnostics, &truncated);
                return Err(command_failed());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn collect_output(reader: &mut impl Read, output: &mut Vec<u8>, truncated: &mut bool) {
    let mut buffer = [0; 4096];
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let retained = count.min(4096 - output.len());
                output.extend_from_slice(&buffer[..retained]);
                *truncated |= retained < count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn log_output(diagnostics: &[Vec<u8>; 2], truncated: &[bool; 2]) {
    for (index, stream) in ["stdout", "stderr"].into_iter().enumerate() {
        if !diagnostics[index].is_empty() || truncated[index] {
            tracing::debug!(
                event = "post_build_hook.output",
                stream,
                truncated = truncated[index],
                output = ?String::from_utf8_lossy(&diagnostics[index]),
                "post-build hook command output"
            );
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "post-build hook configuration is invalid",
    )
}

fn command_failed() -> io::Error {
    io::Error::other("post-build hook command failed")
}
