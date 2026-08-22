use std::io;

const MAXIMUM_DERIVATION_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_COLLECTION_ITEMS: usize = 65_536;
const MAXIMUM_NESTING: usize = 16;

#[derive(Debug)]
pub(super) struct StoredDerivation {
    pub outputs: Vec<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)>,
    pub input_derivations: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
    pub input_sources: Vec<Vec<u8>>,
    pub system: Vec<u8>,
    pub builder: Vec<u8>,
    pub arguments: Vec<Vec<u8>>,
    pub environment: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Debug)]
enum Value {
    String(Vec<u8>),
    List(Vec<Value>),
    Tuple(Vec<Value>),
}

pub(super) fn parse(input: &[u8]) -> io::Result<StoredDerivation> {
    if input.len() > MAXIMUM_DERIVATION_BYTES || !input.starts_with(b"Derive(") {
        return Err(invalid());
    }
    let mut parser = Parser { input, position: 7 };
    let fields = parser.sequence(b')', 0)?;
    if parser.position != input.len() || fields.len() != 7 {
        return Err(invalid());
    }
    let mut fields = fields.into_iter();
    let outputs = tuples(fields.next().ok_or_else(invalid)?, 4)
        .map_err(|_| invalid_field("outputs"))?
        .into_iter()
        .map(|mut output| {
            Ok((
                string(output.remove(0))?,
                string(output.remove(0))?,
                string(output.remove(0))?,
                string(output.remove(0))?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let input_derivations = tuples_with_minimum_width(fields.next().ok_or_else(invalid)?, 2)
        .map_err(|_| invalid_field("input derivations"))?
        .into_iter()
        .map(|mut input| {
            Ok((
                string(input.remove(0))?,
                strings(input.remove(0)).map_err(|_| invalid_field("input derivation outputs"))?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let input_sources =
        strings(fields.next().ok_or_else(invalid)?).map_err(|_| invalid_field("input sources"))?;
    let system = string(fields.next().ok_or_else(invalid)?).map_err(|_| invalid_field("system"))?;
    let builder =
        string(fields.next().ok_or_else(invalid)?).map_err(|_| invalid_field("builder"))?;
    let arguments =
        strings(fields.next().ok_or_else(invalid)?).map_err(|_| invalid_field("arguments"))?;
    let environment = tuples(fields.next().ok_or_else(invalid)?, 2)
        .map_err(|_| invalid_field("environment"))?
        .into_iter()
        .map(|mut entry| Ok((string(entry.remove(0))?, string(entry.remove(0))?)))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(StoredDerivation {
        outputs,
        input_derivations,
        input_sources,
        system,
        builder,
        arguments,
        environment,
    })
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn value(&mut self, depth: usize) -> io::Result<Value> {
        if depth > MAXIMUM_NESTING {
            return Err(invalid());
        }
        match self.peek() {
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => {
                self.position += 1;
                self.sequence(b']', depth + 1).map(Value::List)
            }
            Some(b'(') => {
                self.position += 1;
                self.sequence(b')', depth + 1).map(Value::Tuple)
            }
            _ => Err(invalid()),
        }
    }

    fn sequence(&mut self, terminator: u8, depth: usize) -> io::Result<Vec<Value>> {
        let mut values = Vec::new();
        if self.peek() == Some(terminator) {
            self.position += 1;
            return Ok(values);
        }
        loop {
            if values.len() >= MAXIMUM_COLLECTION_ITEMS {
                return Err(invalid());
            }
            values.push(self.value(depth)?);
            match self.peek() {
                Some(value) if value == terminator => {
                    self.position += 1;
                    return Ok(values);
                }
                Some(b',') => self.position += 1,
                _ => return Err(invalid()),
            }
        }
    }

    fn string(&mut self) -> io::Result<Vec<u8>> {
        if self.peek() != Some(b'"') {
            return Err(invalid());
        }
        self.position += 1;
        let mut value = Vec::new();
        loop {
            let byte = self.take().ok_or_else(invalid)?;
            match byte {
                b'"' => return Ok(value),
                b'\\' => {
                    let escaped = self.take().ok_or_else(invalid)?;
                    value.push(match escaped {
                        b'"' => b'"',
                        b'\\' => b'\\',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        _ => return Err(invalid()),
                    });
                }
                0..=0x1f => return Err(invalid()),
                _ => value.push(byte),
            }
            if value.len() > MAXIMUM_DERIVATION_BYTES {
                return Err(invalid());
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let value = self.peek()?;
        self.position += 1;
        Some(value)
    }
}

fn tuples(value: Value, width: usize) -> io::Result<Vec<Vec<Value>>> {
    tuples_with_width(value, width, |length| length == width)
}

fn tuples_with_minimum_width(value: Value, width: usize) -> io::Result<Vec<Vec<Value>>> {
    tuples_with_width(value, width, |length| length >= width)
}

fn tuples_with_width(
    value: Value,
    _width: usize,
    valid_width: impl Fn(usize) -> bool,
) -> io::Result<Vec<Vec<Value>>> {
    let Value::List(values) = value else {
        return Err(invalid());
    };
    values
        .into_iter()
        .map(|value| match value {
            Value::Tuple(values) if valid_width(values.len()) => Ok(values),
            _ => Err(invalid()),
        })
        .collect()
}

fn strings(value: Value) -> io::Result<Vec<Vec<u8>>> {
    let Value::List(values) = value else {
        return Err(invalid());
    };
    values.into_iter().map(string).collect()
}

fn string(value: Value) -> io::Result<Vec<u8>> {
    match value {
        Value::String(value) => Ok(value),
        _ => Err(invalid()),
    }
}

fn invalid_field(field: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid stored derivation {field}"),
    )
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid stored derivation")
}
