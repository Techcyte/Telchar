//! Delivers immutable archives to operator-owned storage.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};

mod s3;
pub use s3::S3Archive;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ArchiveReceipt {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

pub fn maintain_history(
    database: &(impl crate::persistence::DatabaseSource + ?Sized),
    policy: Option<&crate::service::config::HistoryRetention>,
    cursor: &mut crate::persistence::HistoryCursor,
) -> io::Result<u64> {
    let Some(policy) = policy else { return Ok(0) };
    let destination = policy
        .destination
        .as_ref()
        .map(|path| FilesystemArchive::new(path.clone()))
        .transpose()?;
    let s3 = policy
        .s3
        .as_ref()
        .map(|config| {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Credentials {
                access_key_id: String,
                secret_access_key: String,
                session_token: Option<String>,
            }
            use std::os::unix::fs::MetadataExt;
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(OFlags::NONBLOCK.bits() as i32)
                .open(&config.credentials_file)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.mode() & 0o077 != 0
                || (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
                || metadata.len() > 16384
            {
                return Err(io::Error::other("invalid archive credentials file"));
            }
            let mut bytes = Vec::new();
            (&mut file).take(16385).read_to_end(&mut bytes)?;
            let credentials: Credentials = serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::other("invalid archive credentials file"))?;
            S3Archive::new(
                &config.endpoint,
                &config.region,
                aws_credential_types::Credentials::new(
                    credentials.access_key_id,
                    credentials.secret_access_key,
                    credentials.session_token,
                    None,
                    "archive-file",
                ),
                config.timeout,
            )
        })
        .transpose()?;
    crate::persistence::sweep_terminal_history(
        database,
        policy.retention,
        policy.maximum_builds,
        cursor,
        |key, payload| match &destination {
            Some(archive) => archive.deliver(key, payload).map(|_| ()),
            None => match &s3 {
                Some(archive) => archive.deliver(key, payload),
                None => Ok(()),
            },
        },
    )
}

pub struct FilesystemArchive {
    root: File,
    path: PathBuf,
}

impl FilesystemArchive {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
            .open(&path)?;
        Ok(Self { root, path })
    }

    pub fn deliver(&self, key: &str, payload: &[u8]) -> io::Result<ArchiveReceipt> {
        let components: Vec<_> = key.split('/').collect();
        if components.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid archive key",
            ));
        }
        let mut directory = self.root.try_clone()?;
        for component in &components[..components.len() - 1] {
            match rustix::fs::mkdirat(&directory, *component, Mode::RWXU) {
                Ok(()) => directory.sync_all()?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            directory = File::from(rustix::fs::openat(
                &directory,
                *component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
        }
        // Descriptor-relative paths keep publication anchored even if a directory is renamed.
        let directory_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let destination = directory_path.join(components.last().unwrap());
        let mut temporary = tempfile::NamedTempFile::new_in(&directory_path)?;
        temporary.write_all(payload)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = OpenOptions::new()
                    .read(true)
                    .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
                    .open(&destination)?;
                if !existing.metadata()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "archive is not a regular file",
                    ));
                }
                let mut bytes = Vec::new();
                existing
                    .take(payload.len() as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes != payload {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "archive content conflict",
                    ));
                }
            }
            Err(error) => return Err(error.error),
        }
        directory.sync_all()?;
        Ok(ArchiveReceipt {
            path: self.path.join(key),
            sha256: Sha256::digest(payload)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            bytes: payload.len() as u64,
        })
    }
}
