//! Bounded bootstrap listener behind operator-owned TLS termination.

use std::io;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::rendezvous::{ExecutionSessions, PendingConnections};
use crate::service::singleton_ownership::SingletonOwnership;

pub struct CallbackService {
    stopped: Arc<AtomicBool>,
    listener: Option<JoinHandle<io::Result<()>>>,
    failed: bool,
}

struct Admission {
    worker: JoinHandle<()>,
}

impl CallbackService {
    pub fn start(
        listener: TcpListener,
        owner: Arc<SingletonOwnership>,
        sessions: Arc<ExecutionSessions>,
        maximum_connections: usize,
        maximum_backend_connections: usize,
        admission_timeout: Duration,
    ) -> io::Result<Self> {
        let pending = Arc::new(
            PendingConnections::new(maximum_connections, maximum_backend_connections)
                .map_err(io::Error::other)?,
        );
        if admission_timeout.is_zero() || Instant::now().checked_add(admission_timeout).is_none() {
            return Err(io::Error::other("MicroVM admission timeout is invalid"));
        }
        owner.verify().map_err(io::Error::other)?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stopped);
        let listener = std::thread::Builder::new()
            .name("microvm-listener".into())
            .spawn(move || {
                let mut admissions: Vec<Admission> = Vec::new();
                let mut next_authority_check = Instant::now();
                let result = (|| {
                    while !stopping.load(Ordering::Acquire) {
                        if Instant::now() >= next_authority_check {
                            owner.verify().map_err(io::Error::other)?;
                            next_authority_check = Instant::now() + Duration::from_millis(100);
                        }
                        let mut index = 0;
                        while index < admissions.len() {
                            if admissions[index].worker.is_finished() {
                                admissions.swap_remove(index).worker.join().map_err(|_| {
                                    io::Error::other("MicroVM admission worker failed")
                                })?;
                            } else {
                                index += 1;
                            }
                        }
                        let (stream, _) = match listener.accept() {
                            Ok(connection) => connection,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        if admissions.len() >= maximum_connections {
                            crate::service::metrics::microvm_admission(Duration::ZERO, "capacity");
                            crate::service::metrics::microvm_admission_rejection("capacity");
                            continue;
                        }
                        let owner = Arc::clone(&owner);
                        let sessions = Arc::clone(&sessions);
                        let pending = Arc::clone(&pending);
                        let deadline =
                            Instant::now()
                                .checked_add(admission_timeout)
                                .ok_or_else(|| {
                                    io::Error::other("MicroVM admission timeout is invalid")
                                })?;
                        let worker = std::thread::Builder::new()
                            .name("microvm-admission".into())
                            .spawn(move || {
                                let started = Instant::now();
                                let result = pending.reserve().and_then(|permit| {
                                    sessions.accept(stream, &owner, deadline, permit)
                                });
                                crate::service::metrics::microvm_admission(
                                    started.elapsed(),
                                    if result.is_ok() {
                                        "accepted"
                                    } else {
                                        "rejected"
                                    },
                                );
                                if result.is_err() {
                                    tracing::debug!(event = "microvm.admission.rejected");
                                }
                            })?;
                        admissions.push(Admission { worker });
                    }
                    Ok(())
                })();
                let mut worker_failed = false;
                for admission in admissions {
                    worker_failed |= admission.worker.join().is_err();
                }
                result?;
                if worker_failed {
                    return Err(io::Error::other("MicroVM admission worker failed"));
                }
                Ok(())
            })?;
        Ok(Self {
            stopped,
            listener: Some(listener),
            failed: false,
        })
    }

    pub fn check(&mut self) -> io::Result<()> {
        if self
            .listener
            .as_ref()
            .is_some_and(|listener| listener.is_finished())
        {
            self.join();
        }
        if self.failed || self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::other("MicroVM listener is unavailable"));
        }
        Ok(())
    }

    fn join(&mut self) {
        if let Some(listener) = self.listener.take() {
            self.failed |= !matches!(listener.join(), Ok(Ok(())));
        }
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.join();
        if self.failed {
            return Err(io::Error::other("MicroVM listener failed"));
        }
        Ok(())
    }
}

impl Drop for CallbackService {
    fn drop(&mut self) {
        if self.listener.is_some()
            && let Err(error) = self.shutdown()
        {
            tracing::error!(event = "microvm.listener.shutdown_failed", %error);
        }
    }
}
