//! Verifies grouped public namespaces expose the supported service, store, Nomad, and shared-build APIs.

#[test]
fn grouped_namespaces_expose_domain_apis() {
    let _ = std::mem::size_of::<telchar::service::config::ServiceConfig>();
    let _ = std::mem::size_of::<telchar::store::GatewayStoreEndpoint>();
    let _ = std::mem::size_of::<telchar::nomad::backend::NomadExecutionState>();
    let _ = std::mem::size_of::<telchar::shared_build::SharedBuildRegistry>();
}
