//! Refreshes bounded operator-controlled trusted-key sources.

use super::*;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

struct Snapshot {
    file: KeySource,
    command: KeySource,
    stamp: Option<(SystemTime, u64)>,
    refreshed: Instant,
    unknown_refreshes: std::collections::VecDeque<Instant>,
    keys: BTreeMap<String, RegisteredKey>,
}

pub struct Registry {
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
    resolver: Option<Resolver>,
    static_keys: BTreeMap<String, RegisteredKey>,
}

impl Registry {
    pub fn start(config: &UploadVerification) -> io::Result<Self> {
        config.validate()?;
        let static_keys = config.static_keys()?;
        let state = Arc::new(Mutex::new(Snapshot {
            file: KeySource::default(),
            command: KeySource::default(),
            stamp: None,
            refreshed: Instant::now(),
            unknown_refreshes: Default::default(),
            keys: static_keys.clone(),
        }));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker = if let Some(resolver) = config.resolver.clone() {
            refresh(&mut state.lock().unwrap(), &resolver, &static_keys);
            let state = state.clone();
            let stop = stop.clone();
            let static_keys = static_keys.clone();
            Some(
                std::thread::Builder::new()
                    .name("upload-key-refresh".into())
                    .spawn(move || {
                        loop {
                            let (lock, wake) = &*stop;
                            let guard = lock.lock().unwrap();
                            let (guard, _) = wake
                                .wait_timeout_while(guard, Duration::from_secs(1), |stopped| {
                                    !*stopped
                                })
                                .unwrap();
                            if *guard {
                                break;
                            }
                            drop(guard);
                            let mut snapshot = state.lock().unwrap();
                            let stamp = file_stamp(&resolver);
                            if stamp != snapshot.stamp
                                || snapshot.refreshed.elapsed()
                                    >= parse_duration(&resolver.refresh_interval).unwrap()
                            {
                                refresh(&mut snapshot, &resolver, &static_keys);
                            }
                        }
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            state,
            stop,
            worker,
            resolver: config.resolver.clone(),
            static_keys,
        })
    }

    pub fn lookup(&self, names: &[&str]) -> BTreeMap<String, RegisteredKey> {
        let mut snapshot = self.state.lock().unwrap();
        if names.iter().any(|name| !snapshot.keys.contains_key(*name))
            && let Some(resolver) = &self.resolver
        {
            let now = Instant::now();
            snapshot
                .unknown_refreshes
                .retain(|time| now.duration_since(*time) < Duration::from_secs(60));
            if snapshot.unknown_refreshes.len() < resolver.refresh_limit().unwrap() as usize {
                snapshot.unknown_refreshes.push_back(now);
                refresh(&mut snapshot, resolver, &self.static_keys);
            }
        }
        snapshot.keys.clone()
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap() = true;
        self.stop.1.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn file_stamp(resolver: &Resolver) -> Option<(SystemTime, u64)> {
    resolver
        .file
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())))
}

fn refresh(
    snapshot: &mut Snapshot,
    resolver: &Resolver,
    static_keys: &BTreeMap<String, RegisteredKey>,
) {
    snapshot.stamp = file_stamp(resolver);
    if let Some(path) = &resolver.file {
        let result = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .and_then(|file| {
                if !file.metadata()?.is_file() {
                    return Err(invalid("registry source must be a regular file"));
                }
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                snapshot.file.replace(&bytes, "file")
            });
        if result.is_err() {
            tracing::warn!(event = "registry.refresh_failed", source = "file");
        }
    }
    if let Some(command) = &resolver.command {
        let result = run_command(command, parse_duration(&resolver.command_timeout).unwrap())
            .and_then(|bytes| snapshot.command.replace(&bytes, "command"));
        if result.is_err() {
            tracing::warn!(event = "registry.refresh_failed", source = "command");
        }
    }
    snapshot.keys = merge_keys(static_keys, &[&snapshot.file, &snapshot.command]);
    snapshot.refreshed = Instant::now();
}

pub(super) fn run_command(command: &[String], timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| invalid("registry command failed"))?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid("registry command failed"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| invalid("registry command failed"))?;
        rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK)?;
        rustix::fs::fcntl_setfl(&stderr, rustix::fs::OFlags::NONBLOCK)?;
        let started = Instant::now();
        let mut bytes = Vec::new();
        let mut stderr_bytes = 0u64;
        let mut status = None;
        loop {
            let mut stdout_eof = false;
            let mut buffer = [0; 8192];
            for _ in 0..16 {
                match stdout.read(&mut buffer) {
                    Ok(0) => {
                        stdout_eof = true;
                        break;
                    }
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
                if bytes.len() > 1024 * 1024 {
                    return Err(invalid("registry command output exceeds limit"));
                }
            }
            for _ in 0..16 {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => stderr_bytes = stderr_bytes.saturating_add(count as u64),
                    Err(_) => break,
                }
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status.filter(|_| stdout_eof) {
                if stderr_bytes > 0 {
                    tracing::warn!(
                        event = "registry.command_stderr",
                        bytes = stderr_bytes,
                        "registry command emitted diagnostics; content withheld"
                    );
                }
                return if status.success() {
                    Ok(bytes)
                } else {
                    Err(invalid("registry command failed"))
                };
            }
            if started.elapsed() >= timeout {
                return Err(invalid("registry command timed out"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // Resolver-owned descendants cannot outlive a refresh invocation.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}
