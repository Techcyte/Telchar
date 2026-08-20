//! Defines authenticated frontend-to-daemon envelopes, peer authorization, and bounded stream relaying.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use crate::service::identity::Requester;

pub const MAX_FRONTEND_BUFFER_BYTES: usize = 16 * 1024;

pub fn copy_bounded(mut source: impl Read, mut destination: impl Write) -> io::Result<RelayStats> {
    let mut buffer = [0; MAX_FRONTEND_BUFFER_BYTES];
    let mut maximum_buffered_bytes = 0;
    loop {
        let received = source.read(&mut buffer)?;
        if received == 0 {
            destination.flush()?;
            return Ok(RelayStats {
                maximum_buffered_bytes,
            });
        }
        maximum_buffered_bytes = maximum_buffered_bytes.max(received);
        destination.write_all(&buffer[..received])?;
        destination.flush()?;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayStats {
    pub maximum_buffered_bytes: usize,
}

pub fn relay_bounded(
    mut source: UnixStream,
    mut destination: UnixStream,
) -> io::Result<RelayStats> {
    let _span =
        tracing::info_span!("ipc.stream.relay", buffer_bytes = MAX_FRONTEND_BUFFER_BYTES).entered();
    let stats = copy_bounded(&mut source, &mut destination)?;
    destination.shutdown(std::net::Shutdown::Write)?;
    tracing::info!(
        event = "ipc.stream.relay.completed",
        maximum_buffered_bytes = stats.maximum_buffered_bytes,
        "bounded IPC stream relay completed"
    );
    Ok(stats)
}

pub struct IpcListener {
    listener: UnixListener,
    expected_uid: u32,
}

pub struct IpcConnection {
    stream: UnixStream,
    envelope: IpcEnvelope,
    peer_pid: u32,
}

pub struct PendingIpcConnection {
    stream: UnixStream,
    peer_pid: u32,
}

impl IpcListener {
    pub fn from_listener(listener: UnixListener, expected_uid: u32) -> Self {
        Self {
            listener,
            expected_uid,
        }
    }

    pub fn accept(&self) -> io::Result<IpcConnection> {
        self.accept_with_envelope_timeout(Duration::from_secs(5))
    }

    pub fn accept_with_envelope_timeout(
        &self,
        envelope_timeout: Duration,
    ) -> io::Result<IpcConnection> {
        self.accept_pending()?.receive_envelope(envelope_timeout)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.listener.set_nonblocking(nonblocking)
    }

    pub fn accept_pending(&self) -> io::Result<PendingIpcConnection> {
        let _span = tracing::info_span!("ipc.connection.accept").entered();
        let (stream, _) = self.listener.accept()?;
        authorize_peer(&stream, self.expected_uid)?;
        let peer_pid = peer_pid(&stream)?;
        Ok(PendingIpcConnection { stream, peer_pid })
    }

    pub fn send_envelope(stream: &mut UnixStream, envelope: &IpcEnvelope) -> io::Result<()> {
        let encoded = envelope.encode()?;
        let length = u32::try_from(encoded.len()).map_err(|_| invalid("IPC envelope too large"))?;
        stream.write_all(&length.to_le_bytes())?;
        stream.write_all(&encoded)?;
        stream.flush()
    }

    fn receive_envelope(stream: &mut UnixStream) -> io::Result<IpcEnvelope> {
        let mut length = [0; 4];
        stream.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_IPC_ENVELOPE_BYTES {
            return Err(invalid("IPC envelope exceeds size limit"));
        }
        let mut encoded = vec![0; length];
        stream.read_exact(&mut encoded)?;
        IpcEnvelope::decode(&encoded)
    }
}

impl PendingIpcConnection {
    pub fn receive_envelope(mut self, envelope_timeout: Duration) -> io::Result<IpcConnection> {
        self.stream.set_read_timeout(Some(envelope_timeout))?;
        let envelope =
            IpcListener::receive_envelope(&mut self.stream).map_err(classify_envelope_error)?;
        self.stream.set_read_timeout(None)?;
        tracing::info!(
            event = "ipc.connection.accepted",
            "local IPC connection accepted"
        );
        Ok(IpcConnection {
            stream: self.stream,
            envelope,
            peer_pid: self.peer_pid,
        })
    }
}

impl IpcConnection {
    pub fn envelope(&self) -> &IpcEnvelope {
        &self.envelope
    }
    pub fn stream_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }
    pub fn peer_pid(&self) -> io::Result<u32> {
        Ok(self.peer_pid)
    }
}

#[cfg(target_os = "linux")]
fn peer_pid<Fd: AsFd>(socket: Fd) -> io::Result<u32> {
    Ok(rustix::net::sockopt::socket_peercred(socket)
        .map_err(io::Error::other)?
        .pid
        .as_raw_nonzero()
        .get() as u32)
}

#[cfg(not(target_os = "linux"))]
fn peer_pid<Fd: AsFd>(_socket: Fd) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer PID is unsupported on this platform",
    ))
}

#[cfg(not(target_os = "linux"))]
pub fn authorize_peer<Fd: AsFd>(_socket: Fd, _expected_uid: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local IPC peer credentials are unsupported on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn peer_credentials<Fd: AsFd>(socket: Fd) -> io::Result<libc::ucred> {
    use std::os::fd::AsRawFd;

    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            socket.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local IPC peer credentials have unexpected size",
        ));
    }
    Ok(unsafe { credentials.assume_init() })
}

#[cfg(target_os = "linux")]
pub fn authorize_peer<Fd: AsFd>(socket: Fd, expected_uid: u32) -> io::Result<()> {
    let peer = peer_credentials(socket)?;
    let peer_uid = peer.uid;
    if peer_uid != expected_uid {
        tracing::warn!(
            event = "ipc.peer.rejected",
            reason = "unexpected-uid",
            peer_uid,
            expected_uid
        );
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("local IPC peer uid {peer_uid} does not match expected uid {expected_uid}"),
        ));
    }
    tracing::debug!(event = "ipc.peer.authorized", "local IPC peer authorized");
    Ok(())
}

pub const IPC_VERSION: u16 = 1;
const MAGIC: &[u8; 4] = b"TIPC";
pub const MAX_IPC_COMPONENT_BYTES: usize = 256;
pub const MAX_IPC_CREDENTIAL_ID_BYTES: usize = 1024;
pub const MAX_IPC_ERROR_MESSAGE_BYTES: usize = 4096;
pub const MAX_IPC_ENVELOPE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequesterMetadata {
    pub credential_id: String,
    pub audit_subject: String,
    pub quota_subject: String,
}

impl TryFrom<&Requester> for RequesterMetadata {
    type Error = io::Error;

    fn try_from(requester: &Requester) -> Result<Self, Self::Error> {
        validate_string(&requester.credential_id, MAX_IPC_CREDENTIAL_ID_BYTES)?;
        validate_string(&requester.audit_subject, MAX_IPC_COMPONENT_BYTES)?;
        validate_string(&requester.quota_subject, MAX_IPC_CREDENTIAL_ID_BYTES)?;
        Ok(Self {
            credential_id: requester.credential_id.clone(),
            audit_subject: requester.audit_subject.clone(),
            quota_subject: requester.quota_subject.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcEnvelope {
    pub version: u16,
    pub requester: RequesterMetadata,
    pub session_id: String,
    pub error: Option<IpcError>,
}

impl IpcEnvelope {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let _span = tracing::debug_span!("ipc.envelope.encode", version = self.version).entered();
        if self.version != IPC_VERSION {
            return Err(invalid("unsupported IPC version"));
        }
        let mut output = Vec::with_capacity(128);
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&self.version.to_le_bytes());
        write_string(
            &mut output,
            &self.requester.credential_id,
            MAX_IPC_CREDENTIAL_ID_BYTES,
        )?;
        write_string(
            &mut output,
            &self.requester.audit_subject,
            MAX_IPC_COMPONENT_BYTES,
        )?;
        write_string(
            &mut output,
            &self.requester.quota_subject,
            MAX_IPC_CREDENTIAL_ID_BYTES,
        )?;
        write_string(&mut output, &self.session_id, MAX_IPC_COMPONENT_BYTES)?;
        match &self.error {
            Some(error) => {
                output.push(1);
                write_string(&mut output, &error.code, MAX_IPC_COMPONENT_BYTES)?;
                write_string(&mut output, &error.message, MAX_IPC_ERROR_MESSAGE_BYTES)?;
            }
            None => output.push(0),
        }
        if output.len() > MAX_IPC_ENVELOPE_BYTES {
            return Err(invalid("IPC envelope exceeds size limit"));
        }
        Ok(output)
    }

    pub fn decode(input: &[u8]) -> io::Result<Self> {
        let _span = tracing::debug_span!("ipc.envelope.decode").entered();
        if input.len() > MAX_IPC_ENVELOPE_BYTES {
            return Err(invalid("IPC envelope exceeds size limit"));
        }
        let mut reader = Reader { input, offset: 0 };
        if reader.take(4)? != MAGIC {
            return Err(invalid("invalid IPC envelope magic"));
        }
        let version = reader.u16()?;
        if version != IPC_VERSION {
            return Err(invalid("unsupported IPC version"));
        }
        let requester = RequesterMetadata {
            credential_id: reader.string(MAX_IPC_CREDENTIAL_ID_BYTES)?,
            audit_subject: reader.string(MAX_IPC_COMPONENT_BYTES)?,
            quota_subject: reader.string(MAX_IPC_CREDENTIAL_ID_BYTES)?,
        };
        let session_id = reader.string(MAX_IPC_COMPONENT_BYTES)?;
        let error = match reader.byte()? {
            0 => None,
            1 => Some(IpcError {
                code: reader.string(MAX_IPC_COMPONENT_BYTES)?,
                message: reader.string(MAX_IPC_ERROR_MESSAGE_BYTES)?,
            }),
            _ => return Err(invalid("invalid IPC error flag")),
        };
        if reader.offset != input.len() {
            return Err(invalid("trailing IPC envelope bytes"));
        }
        Ok(Self {
            version,
            requester,
            session_id,
            error,
        })
    }
}

fn write_string(output: &mut Vec<u8>, value: &str, maximum: usize) -> io::Result<()> {
    validate_string(value, maximum)?;
    let length = u16::try_from(value.len()).map_err(|_| invalid("IPC string exceeds bounds"))?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn validate_string(value: &str, maximum: usize) -> io::Result<()> {
    if value.is_empty() || value.len() > maximum {
        return Err(invalid("IPC string exceeds bounds"));
    }
    Ok(())
}

struct Reader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("truncated IPC envelope"))?;
        let value = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated IPC envelope"))?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("length checked"),
        ))
    }

    fn string(&mut self, maximum: usize) -> io::Result<String> {
        let length = usize::from(self.u16()?);
        if length == 0 || length > maximum {
            return Err(invalid("IPC string exceeds bounds"));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| invalid("IPC string is not UTF-8"))
    }
}

fn classify_envelope_error(error: io::Error) -> io::Error {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        tracing::warn!(event = "ipc.envelope.rejected", reason = "timeout");
        io::Error::new(io::ErrorKind::TimedOut, "IPC envelope timed out")
    } else {
        error
    }
}

fn invalid(message: &'static str) -> io::Error {
    tracing::warn!(event = "ipc.envelope.rejected", reason = message);
    io::Error::new(io::ErrorKind::InvalidData, message)
}
