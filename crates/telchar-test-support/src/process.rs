//! Runs environment-sensitive contracts in an exact, isolated test process.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "TELCHAR_TEST_PROCESS";

pub fn is_child(name: &str) -> bool {
    std::env::var_os(CHILD).as_deref() == Some(OsStr::new(name))
}

pub fn command(name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, name);
    command
}

pub fn output(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("captured stdout");
    let mut stderr = child.stderr.take().expect("captured stderr");
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                child.kill()?;
                child.wait()?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "test process deadline expired",
                ));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().expect("stdout reader")?,
        stderr: stderr.join().expect("stderr reader")?,
    })
}

pub fn assert_success(command: &mut Command) -> Output {
    let output = output(command, Duration::from_secs(120)).expect("test process completes");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "exactly one test must run: {output:?}"
    );
    output
}
