use std::io::{self, Read, Write};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarFingerprint {
    pub sha256: [u8; 32],
    pub size: u64,
}

pub fn stage_nar(_source: impl Read, _staging: impl Write) -> io::Result<NarFingerprint> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "NAR staging is not implemented",
    ))
}
