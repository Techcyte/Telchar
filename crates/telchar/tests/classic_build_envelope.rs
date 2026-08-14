//! Tests classic build envelope contracts and failure boundaries, including observes a repeatable typed envelope for each classic build fixture.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use nix_worker_protocol::{CLIENT_WORKER_MAGIC, SERVER_WORKER_MAGIC, STDERR_LAST};

const STDERR_NEXT: u64 = 0x6f6c_6d67;
const STDERR_START_ACTIVITY: u64 = 0x5354_5254;
const STDERR_STOP_ACTIVITY: u64 = 0x5354_4f50;
const STDERR_RESULT: u64 = 0x5253_4c54;
use telchar::fixture::nix::{NixFixture, TrustMode};

const BUFFER_BYTES: usize = 4096;
const CLASSIC_BUILD_EXPRESSION: &str = "derivation { name = \"telchar-classic-fixture\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf telchar-classic-fixture > \\\"$out\\\"\" ]; }";

#[derive(Debug, Eq, PartialEq)]
struct ClassicBuildFixtureEnvelope {
    operation_codes: Vec<u64>,
    maximum_counts: BTreeMap<&'static str, u64>,
    maximum_lengths: BTreeMap<&'static str, u64>,
    maximum_upload_bytes: u64,
}

impl ClassicBuildFixtureEnvelope {
    fn observe(mode: TrustMode) -> io::Result<Self> {
        let fixture = NixFixture::create()?;
        let mut daemon = fixture.start_daemon(mode)?;
        let expected_trust = matches!(mode, TrustMode::Trusted);
        if daemon.trusted()? != expected_trust {
            return Err(invalid("fixture daemon trust result changed"));
        }

        let proxy_path = temporary_socket_path();
        let listener = UnixListener::bind(&proxy_path)?;
        let peer_socket = daemon.socket_path().to_path_buf();
        let observer = thread::spawn(move || -> io::Result<ClassicBuildFixtureEnvelope> {
            let (client, _) = listener.accept()?;
            let peer = UnixStream::connect(peer_socket)?;
            ClassicBuildFixtureEnvelope::relay(client, peer)
        });

        let output = std::process::Command::new(
            std::env::var_os("TELCHAR_NIX_BIN").unwrap_or_else(|| "nix".into()),
        )
        .envs(fixture.environment())
        .args([
            "--store",
            &format!("unix://{}", proxy_path.display()),
            "build",
            "--impure",
            "--expr",
            CLASSIC_BUILD_EXPRESSION,
            "--no-link",
            "--print-out-paths",
        ])
        .output()?;

        let envelope = observer
            .join()
            .map_err(|_| io::Error::other("classic build observer panicked"))??;
        std::fs::remove_file(&proxy_path)?;
        daemon.stop()?;
        fixture.cleanup()?;
        if !output.status.success() {
            return Err(io::Error::other("classic build through observer failed"));
        }
        Ok(envelope)
    }

    fn relay(mut client: UnixStream, mut peer: UnixStream) -> io::Result<Self> {
        let mut envelope = Self {
            operation_codes: Vec::new(),
            maximum_counts: BTreeMap::new(),
            maximum_lengths: BTreeMap::new(),
            maximum_upload_bytes: 0,
        };
        let version = relay_handshake(&mut client, &mut peer, &mut envelope)?;
        relay_post_handshake(&mut client, &mut peer, version, &mut envelope)?;
        relay_stderr_frames(&mut client, &mut peer, &mut envelope)?;

        for expected in [19, 11, 1, 7, 40, 26, 46] {
            let operation = relay_word(&mut client, &mut peer)?;
            if operation != expected {
                return Err(invalid("classic fixture operation sequence changed"));
            }
            envelope.operation_codes.push(operation);
            match operation {
                19 => relay_set_options(&mut client, &mut peer, &mut envelope)?,
                11 | 1 => {
                    relay_store_path(&mut client, &mut peer, "request_store_path", &mut envelope)?
                }
                7 => relay_add_to_store(&mut client, &mut peer, version, &mut envelope)?,
                40 => relay_derived_paths(
                    &mut client,
                    &mut peer,
                    "query_missing_targets",
                    &mut envelope,
                )?,
                26 => relay_store_path(
                    &mut client,
                    &mut peer,
                    "query_path_info_path",
                    &mut envelope,
                )?,
                46 => {
                    relay_derived_paths(&mut client, &mut peer, "build_targets", &mut envelope)?;
                    relay_build_mode(&mut client, &mut peer)?;
                }
                _ => return Err(invalid("untyped classic fixture operation")),
            }
            relay_stderr_frames(&mut client, &mut peer, &mut envelope)?;
            match operation {
                19 => {}
                11 | 1 => {
                    relay_boolean(&mut peer, &mut client)?;
                }
                7 => relay_valid_path_info(&mut peer, &mut client, version, &mut envelope)?,
                40 => {
                    for field in [
                        "missing_will_build",
                        "missing_will_substitute",
                        "missing_unknown",
                    ] {
                        relay_store_path_set(&mut peer, &mut client, field, &mut envelope)?;
                    }
                    relay_word(&mut peer, &mut client)?;
                    relay_word(&mut peer, &mut client)?;
                }
                26 => {
                    if relay_boolean(&mut peer, &mut client)? {
                        relay_unkeyed_valid_path_info(
                            &mut peer,
                            &mut client,
                            version,
                            &mut envelope,
                        )?;
                    }
                }
                46 => relay_keyed_build_results(&mut peer, &mut client, version, &mut envelope)?,
                _ => return Err(invalid("untyped classic fixture response")),
            }
        }
        Ok(envelope)
    }

    fn operation_codes(&self) -> &[u64] {
        &self.operation_codes
    }

    fn merge_maximums(&mut self, other: Self) {
        assert_eq!(self.operation_codes, other.operation_codes);
        for (field, value) in other.maximum_counts {
            record_maximum(&mut self.maximum_counts, field, value);
        }
        for (field, value) in other.maximum_lengths {
            record_maximum(&mut self.maximum_lengths, field, value);
        }
        self.maximum_upload_bytes = self.maximum_upload_bytes.max(other.maximum_upload_bytes);
    }

    fn retains_bodies(&self) -> bool {
        false
    }
}

#[test]
fn observes_a_repeatable_typed_envelope_for_each_classic_build_fixture() -> io::Result<()> {
    for mode in [TrustMode::Trusted, TrustMode::Untrusted] {
        let mut envelope = ClassicBuildFixtureEnvelope::observe(mode)?;
        envelope.merge_maximums(ClassicBuildFixtureEnvelope::observe(mode)?);

        assert_eq!(
            envelope.operation_codes(),
            &[19, 11, 1, 7, 40, 26, 46],
            "fixture operation sequence changed"
        );
        assert!(
            !envelope.retains_bodies(),
            "envelope retained a payload body"
        );
        assert_fixture_envelope(&envelope);
        eprintln!("classic fixture envelope: {envelope:?}");
    }
    Ok(())
}

fn assert_fixture_envelope(envelope: &ClassicBuildFixtureEnvelope) {
    assert_counts_within_fixture_envelope(
        &envelope.maximum_counts,
        BTreeMap::from([
            ("activity_fields", 4),
            ("add_to_store_references", 0),
            ("build_outputs", 1),
            ("build_results", 1),
            ("build_targets", 1),
            ("client_features", 0),
            ("missing_unknown", 0),
            ("missing_will_build", 1),
            ("missing_will_substitute", 0),
            ("option_overrides", 2),
            ("path_info_references", 0),
            ("path_info_signatures", 0),
            ("query_missing_targets", 1),
            ("server_features", 0),
        ]),
    );
    assert_lengths_within_fixture_envelope(
        &envelope.maximum_lengths,
        BTreeMap::from([
            ("activity_field", 153),
            ("activity_message", 164),
            ("add_to_store_content_address", 11),
            ("add_to_store_name", 27),
            ("build_output_id", 75),
            ("build_output_realisation", 196),
            ("build_result_error", 0),
            ("build_result_path", 157),
            ("daemon_version", 6),
            ("derived_path", 157),
            ("option_override_name", 17),
            ("option_override_value", 85),
            ("path_info_content_address", 64),
            ("path_info_deriver", 0),
            ("path_info_nar_hash", 64),
            ("query_path_info_path", 153),
            ("request_store_path", 153),
            ("stderr_message", 145),
            ("store_path", 153),
            ("upload_chunk", 502),
            ("valid_path", 153),
        ]),
    );
    assert!(envelope.maximum_upload_bytes <= 502);
}

fn assert_counts_within_fixture_envelope(
    actual: &BTreeMap<&'static str, u64>,
    limits: BTreeMap<&'static str, u64>,
) {
    for (field, value) in actual {
        assert!(
            *value <= limits[field],
            "fixture count {field} exceeded its acceptance envelope"
        );
    }
}

fn assert_lengths_within_fixture_envelope(
    actual: &BTreeMap<&'static str, u64>,
    limits: BTreeMap<&'static str, u64>,
) {
    for (field, value) in actual {
        assert!(
            *value <= limits[field],
            "fixture length {field} exceeded its acceptance envelope"
        );
    }
}

fn relay_handshake(
    client: &mut UnixStream,
    peer: &mut UnixStream,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<u64> {
    if relay_word(client, peer)? != CLIENT_WORKER_MAGIC
        || relay_word(peer, client)? != SERVER_WORKER_MAGIC
    {
        return Err(invalid("worker handshake magic changed"));
    }
    let server_version = relay_word(peer, client)?;
    let client_version = relay_word(client, peer)?;
    let version = server_version.min(client_version);
    if version != 0x126 {
        return Err(invalid("classic fixture worker version changed"));
    }
    relay_string_set(client, peer, "client_features", envelope)?;
    relay_string_set(peer, client, "server_features", envelope)?;
    Ok(version)
}

fn relay_post_handshake(
    client: &mut UnixStream,
    peer: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    if version >= 0x10e && relay_word(client, peer)? != 0 {
        relay_word(client, peer)?;
    }
    if version >= 0x10b {
        relay_word(client, peer)?;
    }
    if version >= 0x121 {
        relay_string(peer, client, "daemon_version", envelope)?;
    }
    if version >= 0x123 && relay_word(peer, client)? > 2 {
        return Err(invalid("invalid daemon trust status"));
    }
    Ok(())
}

fn relay_set_options(
    client: &mut UnixStream,
    peer: &mut UnixStream,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    for _ in 0..12 {
        relay_word(client, peer)?;
    }
    let count = relay_count(client, peer, "option_overrides", envelope)?;
    for _ in 0..count {
        relay_string(client, peer, "option_override_name", envelope)?;
        relay_string(client, peer, "option_override_value", envelope)?;
    }
    Ok(())
}

fn relay_add_to_store(
    client: &mut UnixStream,
    peer: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    if version < 0x119 {
        return Err(invalid("classic fixture AddToStore version changed"));
    }
    relay_string(client, peer, "add_to_store_name", envelope)?;
    relay_string(client, peer, "add_to_store_content_address", envelope)?;
    relay_store_path_set(client, peer, "add_to_store_references", envelope)?;
    relay_boolean(client, peer)?;
    loop {
        let length = relay_word(client, peer)?;
        if length == 0 {
            break;
        }
        envelope.maximum_upload_bytes = envelope
            .maximum_upload_bytes
            .checked_add(length)
            .ok_or_else(|| invalid("upload length overflow"))?;
        record_maximum(&mut envelope.maximum_lengths, "upload_chunk", length);
        relay_exact(client, peer, length)?;
    }
    Ok(())
}

fn relay_derived_paths(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    count_field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let count = relay_count(source, destination, count_field, envelope)?;
    for _ in 0..count {
        relay_string(source, destination, "derived_path", envelope)?;
    }
    Ok(())
}

fn relay_store_path_set(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    count_field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let count = relay_count(source, destination, count_field, envelope)?;
    for _ in 0..count {
        relay_store_path(source, destination, "store_path", envelope)?;
    }
    Ok(())
}

fn relay_store_path(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    relay_string(source, destination, field, envelope)
}

fn relay_valid_path_info(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    relay_store_path(source, destination, "valid_path", envelope)?;
    relay_unkeyed_valid_path_info(source, destination, version, envelope)
}

fn relay_unkeyed_valid_path_info(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    relay_string(source, destination, "path_info_deriver", envelope)?;
    relay_string(source, destination, "path_info_nar_hash", envelope)?;
    relay_store_path_set(source, destination, "path_info_references", envelope)?;
    relay_word(source, destination)?;
    relay_word(source, destination)?;
    if version >= 0x110 {
        relay_boolean(source, destination)?;
        let count = relay_count(source, destination, "path_info_signatures", envelope)?;
        for _ in 0..count {
            relay_string(source, destination, "path_info_signature", envelope)?;
        }
        relay_string(source, destination, "path_info_content_address", envelope)?;
    }
    Ok(())
}

fn relay_keyed_build_results(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let count = relay_count(source, destination, "build_results", envelope)?;
    for _ in 0..count {
        relay_string(source, destination, "build_result_path", envelope)?;
        relay_build_result(source, destination, version, envelope)?;
    }
    Ok(())
}

fn relay_build_result(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    version: u64,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    if relay_word(source, destination)? > 14 {
        return Err(invalid("invalid build result status"));
    }
    relay_string(source, destination, "build_result_error", envelope)?;
    if version >= 0x11d {
        relay_word(source, destination)?;
        relay_boolean(source, destination)?;
        relay_word(source, destination)?;
        relay_word(source, destination)?;
    }
    if version >= 0x125 {
        relay_optional_duration(source, destination)?;
        relay_optional_duration(source, destination)?;
    }
    if version >= 0x11c {
        let count = relay_count(source, destination, "build_outputs", envelope)?;
        for _ in 0..count {
            relay_string(source, destination, "build_output_id", envelope)?;
            relay_string(source, destination, "build_output_realisation", envelope)?;
        }
    }
    Ok(())
}

fn relay_optional_duration(
    source: &mut UnixStream,
    destination: &mut UnixStream,
) -> io::Result<()> {
    match relay_word(source, destination)? {
        0 => Ok(()),
        1 => {
            relay_word(source, destination)?;
            Ok(())
        }
        _ => Err(invalid("invalid optional duration tag")),
    }
}

fn relay_build_mode(source: &mut UnixStream, destination: &mut UnixStream) -> io::Result<()> {
    if relay_word(source, destination)? > 2 {
        return Err(invalid("invalid build mode"));
    }
    Ok(())
}

fn relay_boolean(source: &mut UnixStream, destination: &mut UnixStream) -> io::Result<bool> {
    match relay_word(source, destination)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("invalid boolean")),
    }
}

fn relay_stderr_frames(
    client: &mut UnixStream,
    daemon: &mut UnixStream,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    loop {
        match relay_word(daemon, client)? {
            STDERR_LAST => return Ok(()),
            STDERR_NEXT => relay_string(daemon, client, "stderr_message", envelope)?,
            STDERR_START_ACTIVITY => {
                relay_word(daemon, client)?;
                relay_word(daemon, client)?;
                relay_word(daemon, client)?;
                relay_string(daemon, client, "activity_message", envelope)?;
                relay_activity_fields(daemon, client, envelope)?;
                relay_word(daemon, client)?;
            }
            STDERR_STOP_ACTIVITY => {
                relay_word(daemon, client)?;
            }
            STDERR_RESULT => {
                relay_word(daemon, client)?;
                relay_word(daemon, client)?;
                relay_activity_fields(daemon, client, envelope)?;
            }
            tag => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unobserved stderr frame: {tag}"),
                ));
            }
        }
    }
}

fn relay_activity_fields(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let count = relay_count(source, destination, "activity_fields", envelope)?;
    for _ in 0..count {
        match relay_word(source, destination)? {
            0 => {
                relay_word(source, destination)?;
            }
            1 => relay_string(source, destination, "activity_field", envelope)?,
            _ => return Err(invalid("unsupported activity field type")),
        }
    }
    Ok(())
}

fn relay_string_set(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    count_field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let count = relay_count(source, destination, count_field, envelope)?;
    for _ in 0..count {
        relay_string(source, destination, "feature", envelope)?;
    }
    Ok(())
}

fn relay_count(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<u64> {
    let count = relay_word(source, destination)?;
    record_maximum(&mut envelope.maximum_counts, field, count);
    Ok(count)
}

fn relay_string(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    field: &'static str,
    envelope: &mut ClassicBuildFixtureEnvelope,
) -> io::Result<()> {
    let length = relay_word(source, destination)?;
    record_maximum(&mut envelope.maximum_lengths, field, length);
    relay_exact(source, destination, length)?;
    let padding = (8 - length % 8) % 8;
    let mut bytes = [0; 7];
    source.read_exact(&mut bytes[..padding as usize])?;
    if bytes[..padding as usize].iter().any(|byte| *byte != 0) {
        return Err(invalid("worker string padding is not zero"));
    }
    destination.write_all(&bytes[..padding as usize])?;
    Ok(())
}

fn relay_word(source: &mut UnixStream, destination: &mut UnixStream) -> io::Result<u64> {
    let mut bytes = [0; 8];
    source.read_exact(&mut bytes)?;
    destination.write_all(&bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn relay_exact(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    mut remaining: u64,
) -> io::Result<()> {
    let mut buffer = [0; BUFFER_BYTES];
    while remaining > 0 {
        let length = usize::try_from(remaining.min(BUFFER_BYTES as u64))
            .map_err(|_| invalid("declared length is not representable"))?;
        source.read_exact(&mut buffer[..length])?;
        destination.write_all(&buffer[..length])?;
        remaining -= length as u64;
    }
    Ok(())
}

fn record_maximum(values: &mut BTreeMap<&'static str, u64>, field: &'static str, value: u64) {
    values
        .entry(field)
        .and_modify(|maximum| *maximum = (*maximum).max(value))
        .or_insert(value);
}

fn temporary_socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tc-envelope-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos()
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
