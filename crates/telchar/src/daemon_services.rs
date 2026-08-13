use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Duration;

const MAINTENANCE_FAILURE: &str = "output retention maintenance failed";
const RECOVERY_FAILURE: &str = "shared build recovery monitor failed";

struct BackgroundService {
    stop: Option<Sender<()>>,
    result: Receiver<io::Result<()>>,
    worker: Option<JoinHandle<()>>,
    failure_message: &'static str,
    state: BackgroundServiceState,
}

impl BackgroundService {
    fn start<F>(interval: Duration, failure_message: &'static str, mut run: F) -> io::Result<Self>
    where
        F: FnMut() -> io::Result<bool> + Send + 'static,
    {
        if interval.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "background service interval must be positive",
            ));
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = loop {
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => match run() {
                        Ok(true) => {}
                        Ok(false) => break Ok(()),
                        Err(_) => break Err(io::Error::other(failure_message)),
                    },
                }
            };
            let _ = result_tx.send(result);
        });
        Ok(Self {
            stop: Some(stop_tx),
            result: result_rx,
            worker: Some(worker),
            failure_message,
            state: BackgroundServiceState::Running,
        })
    }

    fn check(&mut self) -> io::Result<bool> {
        match self.state {
            BackgroundServiceState::Completed => return Ok(false),
            BackgroundServiceState::Failed => {
                return Err(io::Error::other(self.failure_message));
            }
            BackgroundServiceState::Running => {}
        }
        match self.result.try_recv() {
            Ok(Ok(())) => {
                self.state = BackgroundServiceState::Completed;
                Ok(false)
            }
            Err(TryRecvError::Empty) => Ok(true),
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.state = BackgroundServiceState::Failed;
                Err(io::Error::other(self.failure_message))
            }
        }
    }

    fn shutdown(&mut self) -> io::Result<()> {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other(self.failure_message))?;
        }
        self.check().map(|_| ())
    }
}

#[derive(Clone, Copy)]
enum BackgroundServiceState {
    Running,
    Completed,
    Failed,
}

impl Drop for BackgroundService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub struct MaintenanceService(BackgroundService);

impl MaintenanceService {
    pub fn start<F>(interval: Duration, mut maintain: F) -> io::Result<Self>
    where
        F: FnMut() -> io::Result<()> + Send + 'static,
    {
        BackgroundService::start(interval, MAINTENANCE_FAILURE, move || {
            maintain()?;
            Ok(true)
        })
        .map(Self)
    }

    pub fn check(&mut self) -> io::Result<()> {
        self.0.check().map(|_| ())
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.0.shutdown()
    }
}

pub struct RecoveryMonitorService(BackgroundService);

impl RecoveryMonitorService {
    pub fn start<F>(interval: Duration, reconcile: F) -> io::Result<Self>
    where
        F: FnMut() -> io::Result<bool> + Send + 'static,
    {
        BackgroundService::start(interval, RECOVERY_FAILURE, reconcile).map(Self)
    }

    pub fn check(&mut self) -> io::Result<bool> {
        self.0.check()
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.0.shutdown()
    }
}
