//! Captures attempt logs without waiting for compression or storage.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

pub struct LogReceipt {
    pub attempt_id: i64,
    pub path: PathBuf,
    pub bytes: u64,
    pub complete: bool,
}

pub struct AttemptLogs {
    pub root: PathBuf,
    pub maximum_bytes: u64,
    pub maximum_queue_bytes: usize,
    pub maximum_captures: usize,
    active: Arc<AtomicUsize>,
    maximum_spool_bytes: u64,
    retention_days: Option<i64>,
    reservations: Arc<std::sync::Mutex<u64>>,
    destination: Option<PathBuf>,
    s3: Option<crate::service::config::HistoryS3>,
    recovery_cursor: std::sync::Mutex<i64>,
    cleanup_cursor: Arc<std::sync::Mutex<i64>>,
}

impl AttemptLogs {
    pub fn new(
        root: PathBuf,
        maximum_bytes: u64,
        maximum_queue_bytes: usize,
        maximum_captures: usize,
        maximum_spool_bytes: u64,
    ) -> Self {
        let used = (|| {
            let mut used = 0_u64;
            for (index, entry) in std::fs::read_dir(&root).ok()?.enumerate() {
                if index >= 4096 {
                    return None;
                }
                let metadata = entry.ok()?.metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                used = used.checked_add(metadata.len())?;
            }
            Some(used)
        })()
        .unwrap_or(maximum_spool_bytes);
        Self {
            root,
            maximum_bytes,
            maximum_queue_bytes,
            maximum_captures,
            active: Arc::new(AtomicUsize::new(0)),
            maximum_spool_bytes,
            retention_days: None,
            reservations: Arc::new(std::sync::Mutex::new(used)),
            destination: None,
            s3: None,
            recovery_cursor: std::sync::Mutex::new(0),
            cleanup_cursor: Arc::new(std::sync::Mutex::new(0)),
        }
    }

    pub fn with_retention_days(mut self, days: i64) -> Self {
        self.retention_days = Some(days);
        self
    }

    pub fn with_destination(
        mut self,
        directory: Option<PathBuf>,
        s3: Option<crate::service::config::HistoryS3>,
    ) -> Self {
        self.destination = directory;
        self.s3 = s3;
        self
    }

    pub fn reconcile_interrupted(&self, database: &crate::persistence::Database) -> io::Result<()> {
        let mut after = 0;
        loop {
            let attempts = crate::persistence::interrupted_log_archives(database, after)
                .map_err(io::Error::other)?;
            if attempts.is_empty() {
                return Ok(());
            }
            for attempt_id in attempts {
                after = attempt_id;
                self.reclaim_temporary(attempt_id)?;
                let path = self.root.join(format!("{attempt_id}.log.zst"));
                match read_spool(&path, self.maximum_bytes) {
                    Ok(payload) => {
                        use sha2::Digest;
                        let checksum: String = sha2::Sha256::digest(&payload)
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect();
                        crate::persistence::stage_log_archive(
                            database,
                            attempt_id,
                            false,
                            0,
                            &format!("{attempt_id}.log.zst"),
                            &checksum,
                        )
                        .map_err(io::Error::other)?;
                    }
                    Err(_) => crate::persistence::fail_log_archive(database, attempt_id)
                        .map_err(io::Error::other)?,
                }
            }
        }
    }

    fn reclaim_temporary(&self, attempt_id: i64) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let directory = spool_directory(&self.root.join("capture"))?;
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let prefix = format!("attempt-{attempt_id}-");
        for (index, entry) in std::fs::read_dir(&pinned)?.enumerate() {
            if index >= 4096 {
                return Err(io::Error::other("log spool directory exceeds scan bound"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(text) = name.to_str() else {
                continue;
            };
            if !text.starts_with(&prefix) || !text.ends_with(".log.tmp") {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err(io::Error::other("invalid interrupted log spool"));
            }
            rustix::fs::unlinkat(&directory, &name, rustix::fs::AtFlags::empty())?;
            directory.sync_all()?;
            if let Ok(mut reserved) = self.reservations.lock() {
                *reserved = reserved.saturating_sub(metadata.len());
            }
        }
        Ok(())
    }

    fn reclaim_acknowledged(&self, database: &crate::persistence::Database) -> io::Result<()> {
        let mut cursor = self
            .cleanup_cursor
            .lock()
            .map_err(|_| io::Error::other("log cleanup unavailable"))?;
        let entries = crate::persistence::acknowledged_log_spools(database, *cursor)
            .map_err(io::Error::other)?;
        if entries.is_empty() {
            *cursor = 0;
        }
        let mut failure = None;
        for entry in entries {
            *cursor = entry.attempt_id;
            let path = self.root.join(format!("{}.log.zst", entry.attempt_id));
            if entry.spool_name != format!("{}.log.zst", entry.attempt_id)
                || entry.object_key == path.to_string_lossy()
            {
                continue;
            }
            let result = (|| {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return crate::persistence::release_log_spool(database, entry.attempt_id)
                            .map_err(io::Error::other);
                    }
                    Err(error) => return Err(error),
                };
                if !metadata.is_file() {
                    return Err(io::Error::other("invalid acknowledged log spool"));
                }
                release_spool(&path)?;
                if let Ok(mut reserved) = self.reservations.lock() {
                    *reserved = reserved.saturating_sub(metadata.len());
                }
                crate::persistence::release_log_spool(database, entry.attempt_id)
                    .map_err(io::Error::other)
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub fn recover(&self, database: &crate::persistence::Database) -> io::Result<()> {
        if let Some(days) = self.retention_days {
            crate::persistence::expire_local_log_archives(
                database,
                days,
                &self.root.to_string_lossy(),
            )
            .map_err(io::Error::other)?;
        }
        let cleanup_failure = self.reclaim_acknowledged(database).err();
        let mut cursor = self
            .recovery_cursor
            .lock()
            .map_err(|_| io::Error::other("log recovery unavailable"))?;
        let pending = crate::persistence::pending_log_archives(database, *cursor)
            .map_err(io::Error::other)?;
        if pending.is_empty() {
            *cursor = 0;
        }
        let mut failure = cleanup_failure;
        for crate::persistence::PendingLogArchive {
            attempt_id,
            spool_name: name,
            complete,
            uncompressed_bytes: bytes,
            sha256: checksum,
        } in pending
        {
            *cursor = attempt_id;
            let result = (|| {
                if name != format!("{attempt_id}.log.zst") {
                    return Err(io::Error::other("invalid log spool name"));
                }
                let path = self.root.join(&name);
                let payload = read_spool(&path, self.maximum_bytes)?;
                use sha2::Digest;
                let actual: String = sha2::Sha256::digest(&payload)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                if actual != checksum {
                    return Err(io::Error::other("log spool checksum mismatch"));
                }
                let reference = publish_log_file(
                    &self.destination,
                    self.s3.as_ref(),
                    attempt_id,
                    &path,
                    &payload,
                )?;
                crate::persistence::finish_log_archive(
                    database, attempt_id, complete, bytes, &reference, &checksum,
                )
                .map_err(io::Error::other)?;
                if self.destination.is_some() || self.s3.is_some() {
                    release_spool(&path)?;
                    if let Ok(mut reserved) = self.reservations.lock() {
                        *reserved = reserved.saturating_sub(payload.len() as u64);
                    }
                    crate::persistence::release_log_spool(database, attempt_id)
                        .map_err(io::Error::other)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn begin(
        &self,
        database: &crate::persistence::Database,
        attempt_id: i64,
    ) -> Option<LogCapture> {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.maximum_captures).then_some(count + 1)
            })
            .ok()?;
        let reservation = self.maximum_bytes.saturating_mul(2).saturating_add(65536);
        let reserved = (|| {
            let mut reserved = self.reservations.try_lock().ok()?;
            if reserved.checked_add(reservation)? > self.maximum_spool_bytes {
                return None;
            }
            *reserved += reservation;
            Some(())
        })();
        if reserved.is_none() {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let result = (|| {
            if !crate::persistence::begin_log_archive(database, attempt_id).ok()? {
                return None;
            }
            let (capture, finished) = LogCapture::start(
                self.root.clone(),
                attempt_id,
                self.maximum_bytes,
                self.maximum_queue_bytes,
            )
            .ok()?;
            let database = database.clone();
            let active = Arc::clone(&self.active);
            let reservations = Arc::clone(&self.reservations);
            let cleanup = Arc::clone(&self.cleanup_cursor);
            let destination = self.destination.clone();
            let s3 = self.s3.clone();
            let maximum_bytes = self.maximum_bytes;
            std::thread::Builder::new()
                .name("log-receipt".to_owned())
                .spawn(move || {
                    let receipt = finished.recv();
                    let _cleanup = cleanup.lock().unwrap_or_else(|error| error.into_inner());
                    let mut retained = reservation;
                    let result = (|| {
                        let receipt = receipt.map_err(io::Error::other)??;
                        let bytes = read_spool(&receipt.path, maximum_bytes)?;
                        retained = bytes.len() as u64;
                        use sha2::Digest;
                        let checksum: String = sha2::Sha256::digest(&bytes)
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect();
                        crate::persistence::stage_log_archive(
                            &database,
                            attempt_id,
                            receipt.complete,
                            receipt.bytes as i64,
                            receipt
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .ok_or_else(|| io::Error::other("invalid log spool name"))?,
                            &checksum,
                        )
                        .map_err(io::Error::other)?;
                        let reference = publish_log_file(
                            &destination,
                            s3.as_ref(),
                            attempt_id,
                            &receipt.path,
                            &bytes,
                        )?;
                        crate::persistence::finish_log_archive(
                            &database,
                            attempt_id,
                            receipt.complete,
                            receipt.bytes as i64,
                            &reference,
                            &checksum,
                        )
                        .map_err(io::Error::other)?;
                        if destination.is_some() || s3.is_some() {
                            release_spool(&receipt.path)?;
                            retained = 0;
                            crate::persistence::release_log_spool(&database, attempt_id)
                                .map_err(io::Error::other)?;
                        }
                        Ok::<(), io::Error>(())
                    })();
                    if result.is_err() {
                        let _ = crate::persistence::fail_log_archive(&database, attempt_id);
                        tracing::warn!(
                            event = "log.archive.failed",
                            attempt_id,
                            "attempt log archive remains incomplete"
                        );
                    }
                    if let Ok(mut reserved) = reservations.lock() {
                        *reserved = reserved
                            .saturating_sub(reservation)
                            .saturating_add(retained);
                    }
                    active.fetch_sub(1, Ordering::AcqRel);
                })
                .ok()?;
            Some(capture)
        })();
        if result.is_none() {
            if let Ok(mut reserved) = self.reservations.lock() {
                *reserved -= reservation;
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }
}

fn spool_directory(path: &std::path::Path) -> io::Result<std::fs::File> {
    super::open_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("invalid log spool path"))?,
    )
}

fn release_spool(path: &std::path::Path) -> io::Result<()> {
    let directory = spool_directory(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("invalid log spool name"))?;
    rustix::fs::unlinkat(&directory, name, rustix::fs::AtFlags::empty())?;
    directory.sync_all()
}

fn read_spool(path: &std::path::Path, maximum_bytes: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let directory = spool_directory(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("invalid log spool name"))?;
    let file = std::fs::File::from(rustix::fs::openat(
        &directory,
        name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    let limit = maximum_bytes.saturating_mul(2).saturating_add(65536);
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > limit
    {
        return Err(io::Error::other("invalid log spool file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("log spool exceeds bound"));
    }
    Ok(bytes)
}

fn publish_log_file(
    directory: &Option<PathBuf>,
    s3: Option<&crate::service::config::HistoryS3>,
    attempt_id: i64,
    path: &std::path::Path,
    payload: &[u8],
) -> io::Result<String> {
    if directory.is_none() && s3.is_none() {
        return Ok(path.to_string_lossy().into_owned());
    }
    let key = format!("logs/{attempt_id}.log.zst");
    super::deliver_archive(directory.as_deref(), s3, &key, payload)?;
    Ok(match directory {
        Some(directory) => directory.join(&key).to_string_lossy().into_owned(),
        None => format!("{}{}", s3.unwrap().endpoint, key),
    })
}

pub struct LogCapture {
    sender: mpsc::SyncSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    incomplete: Arc<AtomicBool>,
    maximum_queue_bytes: usize,
    ended: Arc<AtomicBool>,
}

impl LogCapture {
    pub fn start(
        root: PathBuf,
        attempt_id: i64,
        maximum_bytes: u64,
        maximum_queue_bytes: usize,
    ) -> io::Result<(Self, mpsc::Receiver<io::Result<LogReceipt>>)> {
        if attempt_id <= 0
            || maximum_bytes == 0
            || maximum_bytes > 1024 * 1024 * 1024
            || maximum_queue_bytes == 0
            || maximum_queue_bytes > 16 * 1024 * 1024
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log capture bounds",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(256);
        let (finished, result) = mpsc::sync_channel(1);
        let queued = Arc::new(AtomicUsize::new(0));
        let incomplete = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        let worker_ended = Arc::clone(&ended);
        let worker_queued = Arc::clone(&queued);
        let worker_incomplete = Arc::clone(&incomplete);
        std::thread::Builder::new()
            .name("attempt-log".to_owned())
            .spawn(move || {
                let capture = (|| {
                    use std::os::fd::AsRawFd;
                    let directory = spool_directory(&root.join("capture"))?;
                    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
                    let mut file = tempfile::Builder::new()
                        .prefix(&format!("attempt-{attempt_id}-"))
                        .suffix(".log.tmp")
                        .tempfile_in(&pinned)?;
                    let mut encoder = zstd::stream::write::Encoder::new(file.as_file_mut(), 3)?;
                    let mut bytes = 0;
                    for chunk in receiver {
                        worker_queued.fetch_sub(chunk.len(), Ordering::AcqRel);
                        let length = (maximum_bytes - bytes).min(chunk.len() as u64) as usize;
                        encoder.write_all(&chunk[..length])?;
                        bytes += length as u64;
                        if length < chunk.len() {
                            worker_incomplete.store(true, Ordering::Release);
                        }
                    }
                    encoder.finish()?;
                    file.as_file().sync_all()?;
                    let path = root.join(format!("{attempt_id}.log.zst"));
                    file.persist_noclobber(pinned.join(format!("{attempt_id}.log.zst")))
                        .map_err(|error| error.error)?;
                    directory.sync_all()?;
                    Ok(LogReceipt {
                        attempt_id,
                        path,
                        bytes,
                        complete: worker_ended.load(Ordering::Acquire)
                            && !worker_incomplete.load(Ordering::Acquire),
                    })
                })();
                let _ = finished.send(capture);
            })?;
        Ok((
            Self {
                sender,
                queued,
                incomplete,
                maximum_queue_bytes,
                ended,
            },
            result,
        ))
    }

    pub fn finish(self) {
        self.ended.store(true, Ordering::Release);
    }

    pub fn push(&self, chunk: &[u8]) {
        for chunk in chunk.chunks(16 * 1024) {
            if self
                .queued
                .try_update(Ordering::AcqRel, Ordering::Acquire, |bytes| {
                    bytes
                        .checked_add(chunk.len())
                        .filter(|bytes| *bytes <= self.maximum_queue_bytes)
                })
                .is_err()
            {
                self.incomplete.store(true, Ordering::Release);
                continue;
            }
            if self.sender.try_send(chunk.to_vec()).is_err() {
                self.queued.fetch_sub(chunk.len(), Ordering::AcqRel);
                self.incomplete.store(true, Ordering::Release);
            }
        }
    }
}
