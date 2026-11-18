//! Bounded operator-controlled upload policy transport; built-in trust checks run first.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAXIMUM_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyHook {
    pub command: Option<Vec<String>>,
    pub url: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Decision {
    pub allow: bool,
    pub reason: Option<String>,
}

fn default_timeout() -> String {
    "2s".into()
}

impl PolicyHook {
    pub fn validate(&self) -> io::Result<()> {
        super::parse_duration(&self.timeout).map_err(|_| invalid())?;
        match (&self.command, &self.url) {
            (Some(command), None) => {
                if command.is_empty()
                    || command.len() > 64
                    || !Path::new(&command[0]).is_absolute()
                    || command
                        .iter()
                        .any(|argument| argument.len() > 4096 || argument.contains('\0'))
                {
                    return Err(invalid());
                }
            }
            (None, Some(url)) => {
                let parsed = url::Url::parse(url).map_err(|_| invalid())?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }

    /// Returns only the external decision. An allow must never override built-in denial.
    /// The caller owns request-schema construction; operator reasons must not contain secrets.
    pub fn invoke(&self, request: &serde_json::Value) -> io::Result<Decision> {
        self.validate()?;
        let deadline =
            Instant::now() + super::parse_duration(&self.timeout).map_err(|_| invalid())?;
        let mut input = serde_json::to_vec(request).map_err(|_| failed())?;
        let bytes = if let Some(command) = &self.command {
            input.push(b'\n');
            run_command(command, &input, deadline)?
        } else {
            invoke_http(self.url.as_deref().ok_or_else(invalid)?, input, deadline)?
        };
        let decision: Decision = serde_json::from_slice(&bytes).map_err(|_| failed())?;
        if decision
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 4096 || reason.chars().any(char::is_control))
        {
            return Err(failed());
        }
        remaining(deadline)?;
        Ok(decision)
    }
}

fn invoke_http(url: &str, input: Vec<u8>, deadline: Instant) -> io::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(remaining(deadline)?)
        .build()
        .map_err(|_| failed())?;
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(remaining(deadline)?)
        .body(input)
        .send()
        .map_err(|_| failed())?;
    // error_for_status alone permits redirects, which are not policy decisions.
    if !response.status().is_success() {
        return Err(failed());
    }
    let mut bytes = Vec::new();
    response
        .take((MAXIMUM_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| failed())?;
    remaining(deadline)?;
    if bytes.len() > MAXIMUM_RESPONSE_BYTES {
        return Err(failed());
    }
    Ok(bytes)
}

struct PolicyProcess {
    child: Child,
}

impl Drop for PolicyProcess {
    fn drop(&mut self) {
        // Descendants may retain pipes even after the direct child exits. Clean up
        // the whole group on success as well as errors; never leave hook jobs alive.
        if let Some(pid) =
            rustix::process::Pid::from_raw(self.child.id() as rustix::process::RawPid)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_command(command: &[String], input: &[u8], deadline: Instant) -> io::Result<Vec<u8>> {
    remaining(deadline)?;
    let mut process = PolicyProcess {
        child: Command::new(&command[0])
            .args(&command[1..])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| failed())?,
    };
    let mut stdin = process.child.stdin.take();
    let mut stdout = process.child.stdout.take().ok_or_else(failed)?;
    let mut stderr = process.child.stderr.take().ok_or_else(failed)?;
    rustix::fs::fcntl_setfl(
        stdin.as_ref().ok_or_else(failed)?,
        rustix::fs::OFlags::NONBLOCK,
    )
    .map_err(|_| failed())?;
    rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK).map_err(|_| failed())?;
    rustix::fs::fcntl_setfl(&stderr, rustix::fs::OFlags::NONBLOCK).map_err(|_| failed())?;
    let mut written = 0;
    let mut output = Vec::new();
    let mut status = None;
    loop {
        remaining(deadline)?;
        let stdout_eof = read_pipe(&mut stdout, Some(&mut output))?;
        let stderr_eof = read_pipe(&mut stderr, None)?;
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&input[written..]) {
                Ok(0) => return Err(failed()),
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
                Err(_) => return Err(failed()),
            }
        }
        if status.is_none() {
            let pid =
                rustix::process::Pid::from_raw(process.child.id() as i32).ok_or_else(failed)?;
            status = rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED
                    | rustix::process::WaitIdOptions::NOHANG
                    | rustix::process::WaitIdOptions::NOWAIT,
            )
            .map_err(|_| failed())?;
        }
        if let Some(status) = status.as_ref() {
            if status.exit_status() != Some(0) || stdin.is_some() {
                return Err(failed());
            }
            if stdout_eof && stderr_eof {
                remaining(deadline)?;
                return Ok(output);
            }
        }
        std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(5)));
    }
}

fn read_pipe(reader: &mut impl Read, mut output: Option<&mut Vec<u8>>) -> io::Result<bool> {
    let mut buffer = [0; 4096];
    // A continuously noisy pipe must not starve stdin, the other pipe, or the deadline.
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if let Some(bytes) = output.as_mut() {
                    if count > MAXIMUM_RESPONSE_BYTES - bytes.len() {
                        return Err(failed());
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(failed()),
        }
    }
    Ok(false)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "upload policy hook timed out"))
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "upload policy hook configuration is invalid",
    )
}

fn failed() -> io::Error {
    io::Error::other("upload policy hook failed")
}

#[cfg(test)]
mod tests;
