//! Tests store closure contracts and failure boundaries, including missing gateway path fails closed through typed daemon query.

use telchar::store_closure::{GatewayStoreClosureBackend, StoreClosureBackend};
use telchar::store_daemon::GatewayStoreEndpoint;

#[test]
fn missing_gateway_path_fails_closed_through_typed_daemon_query() {
    let endpoint = GatewayStoreEndpoint::parse("unix:///definitely-missing/telchar-gateway.sock")
        .expect("endpoint parses");
    let mut backend = GatewayStoreClosureBackend::new(endpoint);

    let error = backend
        .input_closure(&[b"/nix/store/00000000000000000000000000000000-missing".to_vec()])
        .expect_err("missing gateway daemon must fail closed");

    assert_eq!(error.to_string(), "input closure query failed");
}
