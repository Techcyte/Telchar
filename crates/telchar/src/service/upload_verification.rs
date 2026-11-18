//! Client-upload trust configuration and public-key registry.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use base64::Engine;
use serde::{Deserialize, Serialize};

pub mod registry;
pub mod signatures;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Boolean(bool),
    Integer(i64),
    Float(f64),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IdentityMatch {
    #[default]
    Off,
    Warn,
    Enforce,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrustedKey {
    pub public_key: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, MetadataValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredKey {
    pub name: String,
    pub bytes: [u8; 32],
    pub source: &'static str,
    pub detailed: bool,
    pub valid_from: Option<time::OffsetDateTime>,
    pub valid_until: Option<time::OffsetDateTime>,
    pub revoked: bool,
    pub metadata: BTreeMap<String, MetadataValue>,
}

impl TrustedKey {
    pub fn register(&self, source: &'static str, detailed: bool) -> io::Result<RegisteredKey> {
        let (name, encoded) = self
            .public_key
            .split_once(':')
            .ok_or_else(|| invalid("trusted public key is invalid"))?;
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(invalid("trusted public key name is invalid"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("trusted public key is invalid"))?;
        let parse = |value: &str| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid("trusted key validity is invalid"))
        };
        let valid_from = self.valid_from.as_deref().map(parse).transpose()?;
        let valid_until = self.valid_until.as_deref().map(parse).transpose()?;
        if valid_from
            .zip(valid_until)
            .is_some_and(|(from, until)| from >= until)
        {
            return Err(invalid("trusted key validity interval is empty"));
        }
        Ok(RegisteredKey {
            name: name.into(),
            bytes,
            source,
            detailed,
            valid_from,
            valid_until,
            revoked: self.revoked,
            metadata: self.metadata.clone(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UploadVerification {
    pub enabled: bool,
    pub min_signatures: Option<usize>,
    pub identity_match: IdentityMatch,
    pub identity_field: Option<String>,
    pub exempt_content_addressed: Option<bool>,
    pub trusted_public_keys: Vec<String>,
    pub trusted_keys: Vec<TrustedKey>,
    pub resolver: Option<Resolver>,
}

impl UploadVerification {
    pub fn static_keys(&self) -> io::Result<BTreeMap<String, RegisteredKey>> {
        let mut keys: BTreeMap<String, RegisteredKey> = BTreeMap::new();
        for (entry, detailed) in self
            .trusted_public_keys
            .iter()
            .map(|public_key| {
                (
                    TrustedKey {
                        public_key: public_key.clone(),
                        valid_from: None,
                        valid_until: None,
                        revoked: false,
                        metadata: BTreeMap::new(),
                    },
                    false,
                )
            })
            .chain(self.trusted_keys.iter().cloned().map(|entry| (entry, true)))
        {
            let key = entry.register("static", detailed)?;
            if let Some(existing) = keys.get(&key.name) {
                if existing.bytes != key.bytes || (existing.detailed && key.detailed) {
                    return Err(invalid("conflicting static trusted key"));
                }
                if existing.detailed {
                    continue;
                }
            }
            keys.insert(key.name.clone(), key);
        }
        Ok(keys)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.min_signatures.unwrap_or(1) == 0 || self.min_signatures.unwrap_or(1) > 256 {
            return Err(invalid("signature threshold is invalid"));
        }
        if self
            .identity_field
            .as_ref()
            .is_some_and(|field| field.is_empty() || field.len() > 256)
        {
            return Err(invalid("signature identity field is invalid"));
        }
        if self.trusted_public_keys.len() + self.trusted_keys.len() > 4096 {
            return Err(invalid("trusted key count exceeds limit"));
        }
        self.static_keys()?;
        if let Some(resolver) = &self.resolver {
            resolver.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Resolver {
    pub command: Option<Vec<String>>,
    pub file: Option<std::path::PathBuf>,
    #[serde(default = "refresh_interval")]
    pub refresh_interval: String,
    #[serde(default = "command_timeout")]
    pub command_timeout: String,
    #[serde(default = "refresh_limit")]
    pub unknown_key_refresh_limit: String,
}

fn refresh_interval() -> String {
    "5m".into()
}
fn command_timeout() -> String {
    "10s".into()
}
fn refresh_limit() -> String {
    "6/min".into()
}

impl Resolver {
    pub fn validate(&self) -> io::Result<()> {
        parse_duration(&self.refresh_interval)?;
        parse_duration(&self.command_timeout)?;
        self.refresh_limit()?;
        if self.file.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(invalid("registry file path must be absolute"));
        }
        if self.command.as_ref().is_some_and(|command| {
            command.is_empty()
                || command.len() > 64
                || !std::path::Path::new(&command[0]).is_absolute()
                || command
                    .iter()
                    .any(|argument| argument.len() > 4096 || argument.contains('\0'))
        }) {
            return Err(invalid("registry command is invalid"));
        }
        Ok(())
    }

    pub fn refresh_limit(&self) -> io::Result<u32> {
        self.unknown_key_refresh_limit
            .strip_suffix("/min")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|limit| *limit <= 3600)
            .ok_or_else(|| invalid("registry refresh limit is invalid"))
    }
}

fn parse_duration(value: &str) -> io::Result<std::time::Duration> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3600)
    } else {
        return Err(invalid("registry duration is invalid"));
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .filter(|seconds| *seconds > 0 && *seconds <= 86400)
        .map(std::time::Duration::from_secs)
        .ok_or_else(|| invalid("registry duration is invalid"))
}

#[derive(Default)]
pub struct KeySource {
    pub keys: BTreeMap<String, RegisteredKey>,
    pub rejected: BTreeSet<String>,
}

impl KeySource {
    pub fn replace(&mut self, bytes: &[u8], source: &'static str) -> io::Result<()> {
        #[derive(Deserialize)]
        struct Document {
            version: u32,
            keys: Vec<serde_json::Value>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Entry {
            public_key: String,
            valid_from: Option<String>,
            valid_until: Option<String>,
            #[serde(default)]
            revoked: bool,
            #[serde(default)]
            metadata: BTreeMap<String, MetadataValue>,
        }
        if bytes.len() > 1024 * 1024 {
            return Err(invalid("registry source exceeds limit"));
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|_| invalid("registry document is invalid"))?;
        if document.version != 1 || document.keys.len() > 4096 {
            return Err(invalid("registry version or key count is invalid"));
        }
        let mut replacement = Self::default();
        for value in document.keys {
            let key = serde_json::from_value::<Entry>(value)
                .ok()
                .and_then(|entry| {
                    TrustedKey {
                        public_key: entry.public_key,
                        valid_from: entry.valid_from,
                        valid_until: entry.valid_until,
                        revoked: entry.revoked,
                        metadata: entry.metadata,
                    }
                    .register(source, true)
                    .ok()
                });
            let Some(key) = key else {
                tracing::warn!(
                    event = "registry.entry_rejected",
                    source,
                    "invalid registry entry skipped"
                );
                continue;
            };
            if replacement.rejected.contains(&key.name) {
                continue;
            }
            if replacement.keys.contains_key(&key.name) {
                replacement.keys.remove(&key.name);
                replacement.rejected.insert(key.name.clone());
                tracing::warn!(event = "registry.conflict", key_name = key.name, source);
            } else {
                replacement.keys.insert(key.name.clone(), key);
            }
        }
        if replacement.keys.is_empty()
            && replacement.rejected.is_empty()
            && (!self.keys.is_empty() || !self.rejected.is_empty())
        {
            return Err(invalid("registry source unexpectedly returned no keys"));
        }
        *self = replacement;
        Ok(())
    }
}

pub fn merge_keys(
    static_keys: &BTreeMap<String, RegisteredKey>,
    sources: &[&KeySource],
) -> BTreeMap<String, RegisteredKey> {
    let mut keys = static_keys.clone();
    let mut conflicts = sources
        .iter()
        .flat_map(|source| source.rejected.iter().cloned())
        .collect::<BTreeSet<_>>();
    for source in sources {
        for (name, key) in &source.keys {
            if conflicts.contains(name) {
                continue;
            }
            if let Some(existing) = keys.get(name) {
                if existing.bytes != key.bytes || (existing.detailed && key.detailed) {
                    conflicts.insert(name.clone());
                    tracing::warn!(
                        event = "registry.conflict",
                        key_name = name,
                        source = key.source
                    );
                    continue;
                }
                if existing.detailed {
                    continue;
                }
            }
            keys.insert(name.clone(), key.clone());
        }
    }
    keys.retain(|name, _| !conflicts.contains(name));
    keys
}

#[cfg(test)]
mod tests;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
