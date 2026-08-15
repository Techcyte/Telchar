//! Tests store retention contracts and failure boundaries, including empty retention set does not connect to daemon.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, SystemTime};

use telchar::fixture::nix::{NixDaemon, NixFixture, TrustMode};

mod support;

use support::postgres::PostgresFixture;
use telchar::store::retention::{
    NixStoreRetentionBackend, ReleasedRetentionEntry, RetentionEntry, StoreRetentionBackend,
};

#[path = "store_retention/durability.rs"]
mod durability;
#[path = "store_retention/reconciliation.rs"]
mod reconciliation;
#[path = "store_retention/release.rs"]
mod release;

fn retain_fixture_path(
    _fixture: &NixFixture,
    store_uri: &str,
    root_directory: &std::path::Path,
    lease_id: &str,
    store_path: &std::path::Path,
) -> std::io::Result<()> {
    let mut backend = NixStoreRetentionBackend::new_with_store_directory(
        store_uri,
        store_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "store path has no directory",
            )
        })?,
        root_directory,
    )?;
    backend
        .retain(&[RetentionEntry::new(lease_id, store_path.to_string_lossy())])
        .map(|_| ())
}

fn build_fixture_path(
    fixture: &NixFixture,
    daemon: &NixDaemon,
    name: &str,
    contents: &str,
) -> std::path::PathBuf {
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {contents} > \\\"$out\\\"\" ]; }}"
    );
    let output = Command::new("nix")
        .envs(fixture.environment())
        .args([
            "--store",
            &daemon.store_url(),
            "build",
            "--impure",
            "--expr",
            &expression,
            "--no-link",
            "--print-out-paths",
        ])
        .output()
        .expect("fixture derivation builds");
    assert!(
        output.status.success(),
        "fixture derivation failed: {output:?}"
    );
    std::path::PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("output path is UTF-8")
            .trim(),
    )
}
