mod telemetry;

use std::io::{self, Write};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("serve-stdio") {
        serve_stdio();
        return;
    }

    let telemetry = telemetry::Telemetry::initialize()
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
        if std::env::var_os("TELCHAR_SMOKE_ERROR").is_some() {
            tracing::error!(event = "smoke.error", request_id = %request_id, "smoke error");
        }
    }
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
}

fn serve_stdio() {
    let telemetry = telemetry::Telemetry::initialize()
        .expect("telemetry configuration must initialize before application work");
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    let result = nix_worker_protocol::perform_server_handshake(&mut input, &mut output, &[])
        .and_then(|negotiated| {
            nix_worker_protocol::complete_server_post_handshake(
                &mut input,
                &mut output,
                negotiated.version,
                "telchar",
            )
        })
        .and_then(|_| nix_worker_protocol::read_worker_operation_from(&mut input));
    if let Err(_error) = result {
        tracing::error!(
            event = "worker.operation.rejected",
            rejection = "unknown-operation",
            "worker operation rejected"
        );
        let _ = nix_worker_protocol::write_worker_error(&mut output, "unknown worker operation");
        let _ = output.flush();
    }

    telemetry.shutdown();
}
