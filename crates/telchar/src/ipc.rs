use std::io;

pub const IPC_VERSION: u16 = 1;
const MAGIC: &[u8; 4] = b"TIPC";
pub const MAX_IPC_COMPONENT_BYTES: usize = 256;
pub const MAX_IPC_ERROR_MESSAGE_BYTES: usize = 4096;
pub const MAX_IPC_ENVELOPE_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequesterMetadata {
    pub credential_id: String,
    pub audit_subject: String,
    pub quota_subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAttachment {
    pub id: u64,
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
    pub attachment: StreamAttachment,
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
            MAX_IPC_COMPONENT_BYTES,
        )?;
        write_string(
            &mut output,
            &self.requester.audit_subject,
            MAX_IPC_COMPONENT_BYTES,
        )?;
        write_string(
            &mut output,
            &self.requester.quota_subject,
            MAX_IPC_COMPONENT_BYTES,
        )?;
        write_string(&mut output, &self.session_id, MAX_IPC_COMPONENT_BYTES)?;
        output.extend_from_slice(&self.attachment.id.to_le_bytes());
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
            credential_id: reader.string(MAX_IPC_COMPONENT_BYTES)?,
            audit_subject: reader.string(MAX_IPC_COMPONENT_BYTES)?,
            quota_subject: reader.string(MAX_IPC_COMPONENT_BYTES)?,
        };
        let session_id = reader.string(MAX_IPC_COMPONENT_BYTES)?;
        let attachment = StreamAttachment { id: reader.u64()? };
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
            attachment,
            error,
        })
    }
}

fn write_string(output: &mut Vec<u8>, value: &str, maximum: usize) -> io::Result<()> {
    if value.is_empty() || value.len() > maximum || !value.is_char_boundary(value.len()) {
        return Err(invalid("IPC string exceeds bounds"));
    }
    let length = u16::try_from(value.len()).map_err(|_| invalid("IPC string exceeds bounds"))?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
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

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("length checked"),
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

fn invalid(message: &'static str) -> io::Error {
    tracing::warn!(event = "ipc.envelope.rejected", reason = message);
    io::Error::new(io::ErrorKind::InvalidData, message)
}
