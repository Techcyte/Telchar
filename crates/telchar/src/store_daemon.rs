use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::net::Shutdown;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use nix_worker_protocol::{WorkerClient, WorkerClientProfile};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTION_ERROR: &str = "gateway Nix daemon connection failed";

#[derive(Clone, Eq, PartialEq)]
pub struct GatewayStoreEndpoint {
    socket_path: PathBuf,
}

impl GatewayStoreEndpoint {
    pub fn parse(value: &str) -> io::Result<Self> {
        Self::parse_os(OsStr::new(value))
    }

    pub fn parse_os(value: &OsStr) -> io::Result<Self> {
        const PREFIX: &[u8] = b"unix://";
        let value = value.to_str().ok_or_else(connection_error)?.as_bytes();
        let path = value.strip_prefix(PREFIX).ok_or_else(connection_error)?;
        if path.len() <= 1
            || path[0] != b'/'
            || path.starts_with(b"//")
            || path.contains(&b'?')
            || path.contains(&b'#')
            || path.contains(&0)
        {
            return Err(connection_error());
        }
        Ok(Self {
            socket_path: PathBuf::from(OsStr::from_bytes(path)),
        })
    }
}

impl fmt::Debug for GatewayStoreEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayStoreEndpoint")
            .finish_non_exhaustive()
    }
}

pub struct GatewayStoreConnection {
    client: WorkerClient<UnixStream>,
    shutdown_stream: UnixStream,
}

impl GatewayStoreConnection {
    pub fn connect(endpoint: &GatewayStoreEndpoint) -> io::Result<Self> {
        Self::connect_with_timeout(endpoint, OPERATION_TIMEOUT)
    }

    #[doc(hidden)]
    pub fn connect_with_timeout(
        endpoint: &GatewayStoreEndpoint,
        timeout: Duration,
    ) -> io::Result<Self> {
        if timeout.is_zero() {
            return Err(connection_error());
        }
        let stream = UnixStream::connect(&endpoint.socket_path).map_err(|_| connection_error())?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|_| connection_error())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|_| connection_error())?;
        let shutdown_stream = stream.try_clone().map_err(|_| connection_error())?;
        match WorkerClient::connect(stream) {
            Ok(client) => Ok(Self {
                client,
                shutdown_stream,
            }),
            Err(_) => {
                let _ = shutdown_stream.shutdown(Shutdown::Both);
                Err(connection_error())
            }
        }
    }

    pub fn profile(&self) -> &WorkerClientProfile {
        self.client.profile()
    }
}

impl Drop for GatewayStoreConnection {
    fn drop(&mut self) {
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
    }
}

fn connection_error() -> io::Error {
    io::Error::other(CONNECTION_ERROR)
}
