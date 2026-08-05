mod telemetry;

fn main() {
    let local_format = std::env::var_os("TELCHAR_LOCAL_FORMAT").is_some();
    let telemetry = telemetry::Telemetry::initialize(local_format)
        .expect("telemetry configuration must initialize before application work");

    tracing::info!(event = "application.started", "application started");
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
}
