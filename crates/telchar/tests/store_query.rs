//! Tests bounded gateway-store valid-path queries.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use telchar::store::query::{GatewayStoreQuery, QueryValidPathsStore};

#[test]
fn accepts_response_for_realistic_closure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let helper = directory.path().join("nix");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '{'\nseparator=\nfor path in \"$@\"; do\n  case \"$path\" in\n    /nix/store/*) printf '%s\"%s\":{}' \"$separator\" \"$path\"; separator=, ;;\n  esac\ndone\nprintf '}\\n'\n",
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper is executable");
    let endpoint = telchar::store::GatewayStoreEndpoint::parse("unix:///run/nix-daemon.sock")
        .expect("endpoint parses");
    let paths = (0..2_530)
        .map(|index| format!("/nix/store/{index:032x}-realistic-closure-path-{index}").into_bytes())
        .collect::<Vec<_>>();
    let mut query = GatewayStoreQuery::new(helper.to_string_lossy(), endpoint);

    let valid = query
        .query_valid_paths(&paths)
        .expect("realistic closure response is accepted");

    assert_eq!(valid.len(), paths.len());
}
