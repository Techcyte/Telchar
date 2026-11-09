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
            string(
                &mut bytes,
                serde_json::to_string(&value)
                    .map_err(io::Error::other)?
                    .as_bytes(),
            );
        } else {
            string(&mut bytes, value);
        }
        bytes.push(b')');
    }
    bytes.extend_from_slice(b"])");
    Ok(bytes)
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
