use super::*;

pub(super) fn valid_endpoint(value: &str, schemes: &[&str]) -> bool {
    value.len() <= MAXIMUM_NOMAD_ENDPOINT_BYTES
        && schemes.iter().any(|scheme| value.starts_with(scheme))
        && value
            .bytes()
            .all(|byte| !byte.is_ascii_control() && !byte.is_ascii_whitespace())
}

pub(super) fn validate_protected_file(path: PathBuf, message: &'static str) -> io::Result<PathBuf> {
    let metadata = validate_regular_file(&path, message)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid(message));
    }
    Ok(path)
}

pub(super) fn validate_public_file(path: PathBuf, message: &'static str) -> io::Result<PathBuf> {
    let metadata = validate_regular_file(&path, message)?;
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(message));
    }
    Ok(path)
}

pub(super) fn validate_backend_capacity(maximum: usize) -> io::Result<()> {
    if maximum == 0 || maximum > MAXIMUM_BACKEND_CONCURRENT_BUILDS {
        return Err(invalid("backend concurrency limit is invalid"));
    }
    Ok(())
}

pub(super) fn valid_ssh_destination(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAXIMUM_SSH_DESTINATION_BYTES
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'@' | b':' | b'[' | b']')
        })
}

pub(super) fn validate_identity_file(path: &Path) -> io::Result<()> {
    let metadata = validate_regular_file(path, "static SSH identity file is invalid")?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid("static SSH identity file permissions are unsafe"));
    }
    Ok(())
}

pub(super) fn validate_known_hosts_file(path: &Path) -> io::Result<()> {
    let metadata = validate_regular_file(path, "static SSH known-hosts file is invalid")?;
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(
            "static SSH known-hosts file permissions are unsafe",
        ));
    }
    if metadata.size() == 0 || metadata.size() > 1024 * 1024 {
        return Err(invalid("static SSH known-hosts file is invalid"));
    }
    let contents =
        fs::read_to_string(path).map_err(|_| invalid("static SSH known-hosts file is invalid"))?;
    let pinned_key = contents.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with('#') && line.split_ascii_whitespace().count() >= 3
    });
    if !pinned_key {
        return Err(invalid(
            "static SSH known-hosts file has no pinned host key",
        ));
    }
    Ok(())
}

pub(super) fn validate_regular_file(
    path: &Path,
    message: &'static str,
) -> io::Result<fs::Metadata> {
    if !path.is_absolute() {
        return Err(invalid(message));
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid(message))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(message));
    }
    Ok(metadata)
}

pub(super) fn validate_executable_file(path: &Path, message: &'static str) -> io::Result<()> {
    let metadata = fs::metadata(path).map_err(|_| invalid(message))?;
    if !path.is_absolute()
        || !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(invalid(message));
    }
    Ok(())
}

pub(super) fn validate_subject(value: String, message: &'static str) -> io::Result<String> {
    if value.is_empty() || value.len() > MAXIMUM_SUBJECT_BYTES {
        return Err(invalid(message));
    }
    Ok(value)
}

pub(super) fn read_secret(path: PathBuf) -> io::Result<String> {
    if !path.is_absolute() {
        return Err(invalid("database URL file path is invalid"));
    }
    let value =
        fs::read_to_string(path).map_err(|_| invalid("database URL file could not be read"))?;
    nonempty(value.trim().to_owned(), "database URL is invalid")
}

pub(super) fn environment_string(name: &'static str) -> io::Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(invalid("environment override is invalid")),
    }
}

pub(super) fn environment_path(name: &'static str) -> io::Result<Option<PathBuf>> {
    match std::env::var_os(name) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.as_os_str().is_empty() {
                return Err(invalid("environment path override is invalid"));
            }
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

pub(super) fn parse_retention(value: &str) -> io::Result<OutputRetention> {
    OutputRetention::parse(value)
}

pub(super) fn parse_positive_u64(value: &str, message: &'static str) -> io::Result<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(message));
    }
    value.parse().map_err(|_| invalid(message))
}

pub(super) fn parse_positive_usize(value: &str, message: &'static str) -> io::Result<usize> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(message));
    }
    value.parse().map_err(|_| invalid(message))
}

pub(super) fn nonempty(value: String, message: &'static str) -> io::Result<String> {
    if value.trim().is_empty() {
        return Err(invalid(message));
    }
    Ok(value)
}

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
