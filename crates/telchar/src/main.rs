mod telemetry;

fn main() {
    let local_format = std::env::var_os("TELCHAR_LOCAL_FORMAT").is_some();
    let telemetry = telemetry::Telemetry::initialize(local_format)
        .expect("telemetry configuration must initialize before application work");

    tracing::info!(event = "application.started", "application started");
    if let Some(request_id) = std::env::var_os("TELCHAR_SMOKE_REQUEST_ID") {
        let request_id = request_id.to_string_lossy();
        let request = tracing::info_span!("request", request_id = %request_id);
        let _entered = request.enter();
        tracing::info!(event = "request.started", request_id = %request_id, "request started");
        drop(_entered);
        drop(request);
        opentelemetry::global::meter("telchar")
            .u64_counter("telchar.smoke.events")
            .build()
            .add(1, &[]);
    }
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
}
