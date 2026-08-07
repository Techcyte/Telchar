use std::io::{self, ErrorKind, Read, Write};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarFingerprint {
    pub sha256: [u8; 32],
    pub size: u64,
}

struct NarReader<R, W> {
    source: R,
    staging: W,
    hasher: Sha256,
    size: u64,
}

impl<R: Read, W: Write> NarReader<R, W> {
    fn new(source: R, staging: W) -> Self {
        Self {
            source,
            staging,
            hasher: Sha256::new(),
            size: 0,
        }
    }

    fn read_exact(&mut self, buffer: &mut [u8]) -> io::Result<()> {
        self.source.read_exact(buffer)?;
        self.staging.write_all(buffer)?;
        self.hasher.update(&*buffer);
        self.size = self
            .size
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "NAR is too large"))?;
        Ok(())
    }

    fn string_length(&mut self) -> io::Result<u64> {
        let mut length = [0; 8];
        self.read_exact(&mut length)?;
        Ok(u64::from_le_bytes(length))
    }

    fn string_equals(&mut self, expected: &[u8]) -> io::Result<()> {
        let length = self.string_length()?;
        if length > 64 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "NAR marker exceeds bounded parser size",
            ));
        }
        if length != expected.len() as u64 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "unexpected NAR string",
            ));
        }

        let mut remaining = expected;
        let mut buffer = [0; 4096];
        while !remaining.is_empty() {
            let count = remaining.len().min(buffer.len());
            self.read_exact(&mut buffer[..count])?;
            if buffer[..count] != remaining[..count] {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "unexpected NAR string",
                ));
            }
            remaining = &remaining[count..];
        }
        self.read_padding(length)
    }

    fn skip_string(&mut self) -> io::Result<u64> {
        let length = self.string_length()?;
        let mut remaining = length;
        let mut buffer = [0; 8192];
        while remaining != 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            self.read_exact(&mut buffer[..count])?;
            remaining -= count as u64;
        }
        self.read_padding(length)?;
        Ok(length)
    }

    fn read_padding(&mut self, length: u64) -> io::Result<()> {
        let padding = (8 - (length % 8)) % 8;
        let mut buffer = [0; 8];
        self.read_exact(&mut buffer[..padding as usize])?;
        if buffer[..padding as usize].iter().any(|byte| *byte != 0) {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "NAR string padding is not zero",
            ));
        }
        Ok(())
    }

    fn parse_node(&mut self, depth: usize) -> io::Result<()> {
        if depth >= 64 {
            return Err(invalid("NAR directory nesting is too deep"));
        }
        self.string_equals(b"(")?;
        self.string_equals(b"type")?;
        let node_type = self.read_string_word()?;
        match node_type.as_slice() {
            b"regular" => self.parse_regular()?,
            b"symlink" => {
                self.string_equals(b"target")?;
                if self.skip_string()? == 0 {
                    return Err(invalid("NAR symlink target is empty"));
                }
            }
            b"directory" => return self.parse_directory(depth),
            _ => return Err(invalid("unknown NAR node type")),
        }
        self.string_equals(b")").map_err(|error| {
            if error.kind() == ErrorKind::InvalidData {
                io::Error::new(ErrorKind::UnexpectedEof, "NAR node is truncated")
            } else {
                error
            }
        })
    }

    fn parse_regular(&mut self) -> io::Result<()> {
        let marker = self.read_string_word()?;
        if marker == b"executable" {
            if !self.read_string_word()?.is_empty() {
                return Err(invalid("NAR executable marker must be empty"));
            }
            if self.read_string_word()? != b"contents" {
                return Err(invalid("regular NAR node lacks contents"));
            }
        } else if marker != b"contents" {
            return Err(invalid("regular NAR node lacks contents"));
        }
        self.copy_string()
    }

    fn parse_directory(&mut self, depth: usize) -> io::Result<()> {
        let mut previous_name = None;
        loop {
            let marker = self.read_string_word()?;
            if marker == b")" {
                return Ok(());
            }
            if marker != b"entry" {
                return Err(invalid("unexpected directory entry"));
            }
            self.string_equals(b"(")?;
            self.string_equals(b"name")?;
            let name = self.read_bounded_string(4096)?;
            if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
                return Err(invalid("invalid NAR directory name"));
            }
            if previous_name.as_deref().is_some_and(|previous| previous >= name.as_slice()) {
                return Err(invalid("NAR directory entries are not strictly sorted"));
            }
            previous_name = Some(name);
            self.string_equals(b"node")?;
            self.parse_node(depth + 1)?;
            self.string_equals(b")")?;
        }
    }

    fn read_string_word(&mut self) -> io::Result<Vec<u8>> {
        self.read_bounded_string(64)
    }

    fn read_bounded_string(&mut self, maximum: u64) -> io::Result<Vec<u8>> {
        let length = self.string_length()?;
        if length > maximum {
            return Err(invalid("NAR string exceeds parser limit"));
        }
        let mut value = vec![0; length as usize];
        self.read_exact(&mut value)?;
        self.read_padding(length)?;
        Ok(value)
    }

    fn copy_string(&mut self) -> io::Result<()> {
        self.skip_string().map(|_| ())
    }

    fn finish(self) -> NarFingerprint {
        NarFingerprint {
            sha256: self.hasher.finalize().into(),
            size: self.size,
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

pub fn stage_nar<R: Read, W: Write>(source: R, staging: W) -> io::Result<NarFingerprint> {
    let mut nar = NarReader::new(source, staging);
    nar.string_equals(b"nix-archive-1")?;
    nar.parse_node(0)?;

    let mut trailing = [0; 1];
    if nar.source.read(&mut trailing)? != 0 {
        return Err(invalid("trailing bytes after NAR"));
    }
    Ok(nar.finish())
}
