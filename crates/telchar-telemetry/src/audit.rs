//! Audit records use bounded background delivery without synchronous stderr locks.

#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use opentelemetry::InstrumentationScope;
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{
    LogBatch, LogExporter, LogProcessor, SdkLogRecord, SdkLogger, SdkLoggerProvider,
};

static LOGGER: OnceLock<SdkLogger> = OnceLock::new();

pub(crate) fn initialize(provider: &SdkLoggerProvider) {
    let _ = LOGGER.set(provider.logger("telchar.audit"));
}

pub fn emit_audit(event: &'static str, json: &str) {
    if let Some(logger) = LOGGER.get() {
        let mut record = logger.create_log_record();
        record.set_target("telchar.audit");
        record.set_event_name(event);
        record.set_severity_number(Severity::Info);
        record.set_severity_text("INFO");
        record.set_body(AnyValue::from(json.to_owned()));
        logger.emit(record);
    }
}

#[derive(Debug)]
pub(crate) struct SharedExporter<E>(pub Arc<E>);

impl<E: LogExporter> LogExporter for SharedExporter<E> {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        self.0.export(batch).await
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }
}

enum Message {
    Record(Box<(SdkLogRecord, InstrumentationScope)>),
    Flush(mpsc::SyncSender<()>),
    Shutdown(mpsc::SyncSender<()>),
}

#[derive(Debug)]
pub(crate) struct AuditProcessor<P> {
    ordinary: P,
    sender: mpsc::SyncSender<Message>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stopping: Arc<AtomicBool>,
}

impl<P: LogProcessor> AuditProcessor<P> {
    pub fn new<E: LogExporter + 'static>(ordinary: P, exporter: Arc<E>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(super::MAX_QUEUE_SIZE);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let message = match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(message) => message,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if stop.load(Ordering::Acquire) {
                    break;
                }
                match message {
                    Message::Record(record) => {
                        let records = [(&record.0, &record.1)];
                        if futures_executor::block_on(exporter.export(LogBatch::new(&records)))
                            .is_err()
                        {
                            fallback(&record.0, "audit.export_failed");
                        }
                    }
                    Message::Flush(done) => {
                        let _ = done.send(());
                    }
                    Message::Shutdown(done) => {
                        let _ = done.send(());
                        break;
                    }
                }
            }
        });
        Self {
            ordinary,
            sender,
            worker: Mutex::new(Some(worker)),
            stopping,
        }
    }
}

impl<P: LogProcessor> LogProcessor for AuditProcessor<P> {
    fn emit(&self, record: &mut SdkLogRecord, scope: &InstrumentationScope) {
        if record
            .target()
            .is_none_or(|target| target != "telchar.audit")
        {
            self.ordinary.emit(record, scope);
            return;
        }
        if let Err(
            mpsc::TrySendError::Full(Message::Record(record))
            | mpsc::TrySendError::Disconnected(Message::Record(record)),
        ) = self
            .sender
            .try_send(Message::Record(Box::new((record.clone(), scope.clone()))))
        {
            fallback(&record.0, "audit.queue_full");
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Message::Flush(sender))
            .map_err(|_| OTelSdkError::Timeout(super::EXPORT_TIMEOUT))?;
        receiver
            .recv_timeout(super::EXPORT_TIMEOUT)
            .map_err(|_| OTelSdkError::Timeout(super::EXPORT_TIMEOUT))?;
        self.ordinary.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        let started = std::time::Instant::now();
        let (sender, receiver) = mpsc::sync_channel(1);
        let audit_result = self
            .sender
            .try_send(Message::Shutdown(sender))
            .map_err(|_| OTelSdkError::Timeout(timeout))
            .and_then(|()| {
                receiver
                    .recv_timeout(timeout / 2)
                    .map_err(|_| OTelSdkError::Timeout(timeout))
            });
        self.stopping.store(true, Ordering::Release);
        let ordinary_result = self
            .ordinary
            .shutdown_with_timeout(timeout.saturating_sub(started.elapsed()));
        if let Ok(mut worker) = self.worker.lock()
            && worker.as_ref().is_some_and(|worker| worker.is_finished())
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
        audit_result.and(ordinary_result)
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.ordinary.set_resource(resource);
    }
}

fn fallback(record: &SdkLogRecord, reason: &str) {
    if let Some(AnyValue::String(body)) = record.body() {
        let event = record.event_name().unwrap_or("audit.event");
        let line = format!("WARN {reason} event={event} {}\n", body.as_str());
        // Opening procfs gives an independent file description: O_NONBLOCK does not alter stderr.
        // A stalled or unavailable local sink cannot hold the process-wide stderr lock.
        unsafe {
            let mut stat = std::mem::zeroed::<libc::stat>();
            if libc::fstat(libc::STDERR_FILENO, &mut stat) == 0
                && stat.st_mode & libc::S_IFMT == libc::S_IFSOCK
            {
                let _ = libc::send(
                    libc::STDERR_FILENO,
                    line.as_ptr().cast(),
                    line.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                );
                return;
            }
            if stat.st_mode & libc::S_IFMT == libc::S_IFREG {
                let flags = libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(libc::STDERR_FILENO, libc::F_SETFL, flags | libc::O_APPEND);
                }
            }
            let fd = libc::open(
                c"/proc/self/fd/2".as_ptr(),
                libc::O_WRONLY | libc::O_APPEND | libc::O_NONBLOCK | libc::O_CLOEXEC,
            );
            if fd >= 0 {
                let _ = libc::write(fd, line.as_ptr().cast(), line.len());
                libc::close(fd);
            }
        }
    }
}
