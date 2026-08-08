use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

pub const DEFAULT_MAXIMUM_NAR_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAXIMUM_INBOUND_NAR_SESSION_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAXIMUM_OUTBOUND_NAR_SESSION_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAXIMUM_NAR_SESSION_OBJECTS: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferLimits {
    pub maximum_object_bytes: u64,
    pub maximum_inbound_session_bytes: u64,
    pub maximum_outbound_session_bytes: u64,
    pub maximum_inbound_session_objects: u64,
    pub maximum_outbound_session_objects: u64,
    pub maximum_active_inbound_objects: u64,
    pub maximum_active_outbound_objects: u64,
}

impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: DEFAULT_MAXIMUM_NAR_OBJECT_BYTES,
            maximum_inbound_session_bytes: DEFAULT_MAXIMUM_INBOUND_NAR_SESSION_BYTES,
            maximum_outbound_session_bytes: DEFAULT_MAXIMUM_OUTBOUND_NAR_SESSION_BYTES,
            maximum_inbound_session_objects: DEFAULT_MAXIMUM_NAR_SESSION_OBJECTS,
            maximum_outbound_session_objects: DEFAULT_MAXIMUM_NAR_SESSION_OBJECTS,
            maximum_active_inbound_objects: DEFAULT_MAXIMUM_NAR_SESSION_OBJECTS,
            maximum_active_outbound_objects: DEFAULT_MAXIMUM_NAR_SESSION_OBJECTS,
        }
    }
}

impl TransferLimits {
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        object: &str,
        inbound: &str,
        outbound: &str,
        inbound_session_objects: &str,
        outbound_session_objects: &str,
        active_inbound_objects: &str,
        active_outbound_objects: &str,
    ) -> io::Result<Self> {
        Ok(Self {
            maximum_object_bytes: positive(object)?,
            maximum_inbound_session_bytes: positive(inbound)?,
            maximum_outbound_session_bytes: positive(outbound)?,
            maximum_inbound_session_objects: positive(inbound_session_objects)?,
            maximum_outbound_session_objects: positive(outbound_session_objects)?,
            maximum_active_inbound_objects: positive(active_inbound_objects)?,
            maximum_active_outbound_objects: positive(active_outbound_objects)?,
        })
    }

    pub fn from_environment() -> io::Result<Self> {
        let defaults = Self::default();
        Self::parse(
            &std::env::var("TELCHAR_MAX_NAR_OBJECT_BYTES")
                .unwrap_or_else(|_| defaults.maximum_object_bytes.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_INBOUND_SESSION_BYTES")
                .unwrap_or_else(|_| defaults.maximum_inbound_session_bytes.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_OUTBOUND_SESSION_BYTES")
                .unwrap_or_else(|_| defaults.maximum_outbound_session_bytes.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_INBOUND_SESSION_OBJECTS")
                .unwrap_or_else(|_| defaults.maximum_inbound_session_objects.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_OUTBOUND_SESSION_OBJECTS")
                .unwrap_or_else(|_| defaults.maximum_outbound_session_objects.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_ACTIVE_INBOUND_OBJECTS")
                .unwrap_or_else(|_| defaults.maximum_active_inbound_objects.to_string()),
            &std::env::var("TELCHAR_MAX_NAR_ACTIVE_OUTBOUND_OBJECTS")
                .unwrap_or_else(|_| defaults.maximum_active_outbound_objects.to_string()),
        )
    }
}

fn positive(value: &str) -> io::Result<u64> {
    let value = value
        .parse::<u64>()
        .map_err(|_| invalid("NAR transfer limit must be a positive integer"))?;
    if value == 0 {
        return Err(invalid("NAR transfer limit must be positive"));
    }
    Ok(value)
}

#[derive(Clone, Debug)]
pub struct ObjectAdmissionState {
    active: Arc<Mutex<ActiveObjects>>,
    inbound_limit: u64,
    outbound_limit: u64,
}

#[derive(Debug, Default)]
struct ActiveObjects {
    inbound: u64,
    outbound: u64,
}

#[derive(Debug, Default)]
pub struct ObjectSessionCounts {
    inbound: u64,
    outbound: u64,
}

impl ObjectSessionCounts {
    pub fn admit_inbound(&mut self, limit: u64) -> io::Result<()> {
        self.inbound = charge_object_count(self.inbound, limit).map_err(|_| {
            reject("inbound", "session", limit);
            invalid("NAR inbound session object limit exceeded")
        })?;
        Ok(())
    }

    pub fn admit_outbound(&mut self, limit: u64) -> io::Result<()> {
        self.outbound = charge_object_count(self.outbound, limit).map_err(|_| {
            reject("outbound", "session", limit);
            invalid("NAR outbound session object limit exceeded")
        })?;
        Ok(())
    }
}

fn charge_object_count(current: u64, limit: u64) -> io::Result<u64> {
    let next = current
        .checked_add(1)
        .ok_or_else(|| invalid("object count exceeded"))?;
    if next > limit {
        return Err(invalid("object count exceeded"));
    }
    Ok(next)
}

impl ObjectAdmissionState {
    pub fn new(limits: &TransferLimits) -> Self {
        Self {
            active: Arc::new(Mutex::new(ActiveObjects::default())),
            inbound_limit: limits.maximum_active_inbound_objects,
            outbound_limit: limits.maximum_active_outbound_objects,
        }
    }

    pub fn admit_inbound(&self) -> io::Result<ActiveObjectPermit> {
        self.admit("inbound", self.inbound_limit, true)
    }

    pub fn admit_outbound(&self) -> io::Result<ActiveObjectPermit> {
        self.admit("outbound", self.outbound_limit, false)
    }

    fn admit(
        &self,
        direction: &'static str,
        limit: u64,
        inbound: bool,
    ) -> io::Result<ActiveObjectPermit> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| io::Error::other("object admission state is unavailable"))?;
        let count = if inbound {
            &mut active.inbound
        } else {
            &mut active.outbound
        };
        let next = count
            .checked_add(1)
            .ok_or_else(|| invalid("active object count exceeded"))?;
        if next > limit {
            reject(direction, "deployment-active", limit);
            return Err(invalid("active object count exceeded"));
        }
        *count = next;
        Ok(ActiveObjectPermit {
            active: Arc::clone(&self.active),
            inbound,
        })
    }
}

#[derive(Debug)]
pub struct ActiveObjectPermit {
    active: Arc<Mutex<ActiveObjects>>,
    inbound: bool,
}

impl Drop for ActiveObjectPermit {
    fn drop(&mut self) {
        let Ok(mut active) = self.active.lock() else {
            tracing::error!(
                event = "worker.transfer.object_admission_unavailable",
                "object admission permit could not be released"
            );
            return;
        };
        let count = if self.inbound {
            &mut active.inbound
        } else {
            &mut active.outbound
        };
        *count = count.saturating_sub(1);
    }
}

#[derive(Debug)]
pub struct TransferBudget {
    limit: u64,
    charged: u64,
}

impl TransferBudget {
    pub fn new(limit: u64) -> Self {
        assert!(limit > 0, "transfer budget limit must be positive");
        Self { limit, charged: 0 }
    }

    pub fn charge(&mut self, amount: usize) -> io::Result<()> {
        let amount = u64::try_from(amount).map_err(|_| invalid("transfer is too large"))?;
        let next = self
            .charged
            .checked_add(amount)
            .ok_or_else(|| invalid("transfer session byte limit exceeded"))?;
        if next > self.limit {
            return Err(invalid("transfer session byte limit exceeded"));
        }
        self.charged = next;
        Ok(())
    }

    pub fn charged(&self) -> u64 {
        self.charged
    }

    fn remaining(&self) -> u64 {
        self.limit - self.charged
    }
}

pub struct LimitedReader<'a, R> {
    source: R,
    object_limit: u64,
    object_charged: u64,
    session: &'a mut TransferBudget,
}

impl<'a, R> LimitedReader<'a, R> {
    pub fn new(source: R, object_limit: u64, session: &'a mut TransferBudget) -> Self {
        Self {
            source,
            object_limit,
            object_charged: 0,
            session,
        }
    }
}

impl<R: Read> Read for LimitedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let object_remaining = self.object_limit - self.object_charged;
        if object_remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.source.read(&mut probe)? != 0 {
                reject("inbound", "object", self.object_limit);
                return Err(invalid("NAR object byte limit exceeded"));
            }
            return Ok(0);
        }
        let session_remaining = self.session.remaining();
        if session_remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.source.read(&mut probe)? != 0 {
                reject("inbound", "session", self.session.limit);
                return Err(invalid("transfer session byte limit exceeded"));
            }
            return Ok(0);
        }
        let requested = buffer
            .len()
            .min(usize::try_from(object_remaining.min(session_remaining)).unwrap_or(usize::MAX));
        let read = self.source.read(&mut buffer[..requested])?;
        if read == 0 {
            return Ok(0);
        }
        self.session.charge(read)?;
        self.object_charged = self
            .object_charged
            .checked_add(u64::try_from(read).map_err(|_| invalid("transfer is too large"))?)
            .ok_or_else(|| invalid("NAR object byte limit exceeded"))?;
        Ok(read)
    }
}

pub struct LimitedWriter<'a, W> {
    sink: W,
    object_limit: u64,
    object_charged: u64,
    session: &'a mut TransferBudget,
}

impl<'a, W> LimitedWriter<'a, W> {
    pub fn new(sink: W, object_limit: u64, session: &'a mut TransferBudget) -> Self {
        Self {
            sink,
            object_limit,
            object_charged: 0,
            session,
        }
    }
}

impl<W: Write> Write for LimitedWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let object_remaining = self.object_limit.saturating_sub(self.object_charged);
        let session_remaining = self.session.remaining();
        let allowed = buffer
            .len()
            .min(usize::try_from(object_remaining.min(session_remaining)).unwrap_or(usize::MAX));
        if session_remaining == 0 {
            reject("outbound", "session", self.session.limit);
            return Err(invalid("transfer session byte limit exceeded"));
        }
        if allowed == 0 {
            reject("outbound", "object", self.object_limit);
            return Err(invalid("NAR object byte limit exceeded"));
        }
        let written = self.sink.write(&buffer[..allowed])?;
        self.session.charge(written)?;
        self.object_charged = self
            .object_charged
            .checked_add(u64::try_from(written).map_err(|_| invalid("transfer is too large"))?)
            .ok_or_else(|| invalid("NAR object byte limit exceeded"))?;
        if allowed < buffer.len() && written == allowed {
            if session_remaining <= object_remaining {
                reject("outbound", "session", self.session.limit);
                return Err(invalid("transfer session byte limit exceeded"));
            }
            reject("outbound", "object", self.object_limit);
            return Err(invalid("NAR object byte limit exceeded"));
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

fn reject(direction: &'static str, scope: &'static str, limit: u64) {
    tracing::warn!(
        event = "worker.transfer.limit_rejected",
        direction,
        scope,
        configured_limit = limit,
        "raw NAR transfer limit rejected"
    );
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
