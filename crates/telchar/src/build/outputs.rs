use std::collections::{BTreeMap, BTreeSet};
use std::io;

use sha2::{Digest, Sha256};

use super::BuildRequest;

// Nix staticOutputHashes hashes the masked BasicDerivation supplied over the
// worker protocol, with realized inputs and no input-derivation provenance.

pub(super) fn realizations(
    build: &BuildRequest,
) -> io::Result<Vec<nix_worker_protocol::BuildOutput>> {
    let masked = masked_derivation(build)?;
    let input_hash = hex(&Sha256::digest(&masked));
    build
        .output_authorities()
        .iter()
        .map(|output| {
            let hash = if output.hash_algorithm().is_empty() {
                input_hash.clone()
            } else {
                let mut identity = b"fixed:out:".to_vec();
                identity.extend_from_slice(output.hash_algorithm());
                identity.push(b':');
                let content_hash = super::decode_hex(output.hash()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid output hash")
                })?;
                identity.extend_from_slice(hex(&content_hash).as_bytes());
                identity.push(b':');
                identity.extend_from_slice(output.path());
                hex(&Sha256::digest(identity))
            };
            nix_worker_protocol::BuildOutput::new(
                format!(
                    "sha256:{hash}!{}",
                    std::str::from_utf8(output.name()).map_err(io::Error::other)?
                ),
                std::str::from_utf8(output.path())
                    .map_err(io::Error::other)?
                    .to_owned(),
            )
        })
        .collect()
}

fn masked_derivation(build: &BuildRequest) -> io::Result<Vec<u8>> {
    let mut bytes = b"Derive([".to_vec();
    let outputs = build
        .output_authorities()
        .iter()
        .map(|output| (output.name(), output))
        .collect::<BTreeMap<_, _>>();
    for (index, (name, output)) in outputs.iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        bytes.push(b'(');
        string(&mut bytes, name);
        bytes.extend_from_slice(b",\"\",");
        string(&mut bytes, output.hash_algorithm());
        bytes.push(b',');
        string(&mut bytes, output.hash());
        bytes.push(b')');
    }
    bytes.extend_from_slice(b"],[],[");
    for (index, source) in build
        .input_sources()
        .iter()
        .collect::<BTreeSet<_>>()
        .iter()
        .enumerate()
    {
        if index > 0 {
            bytes.push(b',');
        }
        string(&mut bytes, source);
    }
    bytes.extend_from_slice(b"],");
    string(&mut bytes, build.system().as_bytes());
    bytes.push(b',');
    string(&mut bytes, build.builder());
    bytes.extend_from_slice(b",[");
    for (index, argument) in build.arguments().iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        string(&mut bytes, argument);
    }
    bytes.extend_from_slice(b"],[");
    for (index, (key, value)) in build
        .environment()
        .iter()
        .map(|(key, value)| (key.as_slice(), value.as_slice()))
        .collect::<BTreeMap<_, _>>()
        .iter()
        .enumerate()
    {
        if index > 0 {
            bytes.push(b',');
        }
        bytes.push(b'(');
        string(&mut bytes, key);
        bytes.push(b',');
        if outputs.contains_key(key) {
            string(&mut bytes, b"");
        } else if *key == b"__json" {
            let value: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(value).map_err(io::Error::other)?;
            let mut encoded = String::new();
            json(&mut encoded, &serde_json::Value::Object(value))?;
            string(&mut bytes, encoded.as_bytes());
        } else {
            string(&mut bytes, value);
        }
        bytes.push(b')');
    }
    bytes.extend_from_slice(b"])");
    Ok(bytes)
}

fn json(output: &mut String, value: &serde_json::Value) -> io::Result<()> {
    match value {
        serde_json::Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                json(output, value)?;
            }
            output.push(']');
        }
        serde_json::Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(io::Error::other)?);
                output.push(':');
                json(output, value)?;
            }
            output.push('}');
        }
        serde_json::Value::Number(number) if number.is_f64() => {
            let mut buffer = dtoa::Buffer::new();
            let number = buffer.format(
                number
                    .as_f64()
                    .ok_or_else(|| io::Error::other("invalid JSON number"))?,
            );
            let (sign, unsigned) = number
                .strip_prefix('-')
                .map(|value| ("-", value))
                .unwrap_or(("", number));
            let (mantissa, exponent) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
            let exponent: i32 = exponent.parse().map_err(io::Error::other)?;
            let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
            let digits = mantissa.replace('.', "");
            let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
            let position = point + exponent - leading as i32;
            let digits = digits[leading..].trim_end_matches('0');
            output.push_str(sign);
            if digits.is_empty() {
                output.push_str("0.0");
            } else if position > 0 && position <= 15 {
                let position = position as usize;
                if position >= digits.len() {
                    output.push_str(digits);
                    output.extend(std::iter::repeat_n('0', position - digits.len()));
                    output.push_str(".0");
                } else {
                    output.push_str(&digits[..position]);
                    output.push('.');
                    output.push_str(&digits[position..]);
                }
            } else if position > -4 && position <= 0 {
                output.push_str("0.");
                output.extend(std::iter::repeat_n('0', -position as usize));
                output.push_str(digits);
            } else {
                output.push_str(&digits[..1]);
                if digits.len() > 1 {
                    output.push('.');
                    output.push_str(&digits[1..]);
                }
                output.push('e');
                let exponent = position - 1;
                output.push(if exponent < 0 { '-' } else { '+' });
                output.push_str(&format!("{:02}", exponent.abs()));
            }
        }
        _ => output.push_str(&serde_json::to_string(value).map_err(io::Error::other)?),
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn string(output: &mut Vec<u8>, value: &[u8]) {
    output.push(b'"');
    for byte in value {
        match byte {
            b'"' | b'\\' => {
                output.push(b'\\');
                output.push(*byte);
            }
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            _ => output.push(*byte),
        }
    }
    output.push(b'"');
}
