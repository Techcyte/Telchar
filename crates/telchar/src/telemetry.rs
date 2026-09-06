//! Verifies gateway telemetry through real collector endpoints.

use telchar_telemetry::Telemetry;
const SERVICE_NAME: &str = "telchar";

#[cfg(test)]
static TELEMETRY_TESTS: std::sync::OnceLock<(std::fs::File, std::sync::Mutex<()>)> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn telemetry_tests() -> &'static std::sync::Mutex<()> {
    &TELEMETRY_TESTS
        .get_or_init(|| {
            let lock_path = std::env::temp_dir().join("telchar-telemetry-tests.lock");
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_path)
                .expect("telemetry test lock file opens");
            rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
                .expect("telemetry test process lock");
            (lock, std::sync::Mutex::new(()))
        })
        .1
}

#[cfg(test)]
#[derive(Clone)]
struct Collector {
    endpoint: String,
    trace_requests: std::sync::Arc<
        std::sync::Mutex<
            Vec<opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest>,
        >,
    >,
    log_requests: std::sync::Arc<
        std::sync::Mutex<
            Vec<opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest>,
        >,
    >,
    metric_requests: std::sync::Arc<
        std::sync::Mutex<
            Vec<opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest>,
        >,
    >,
}

#[cfg(test)]
impl Collector {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn has_all_signals(&self) -> bool {
        !self
            .trace_requests
            .lock()
            .expect("trace requests")
            .is_empty()
            && !self.log_requests.lock().expect("log requests").is_empty()
            && !self
                .metric_requests
                .lock()
                .expect("metric requests")
                .is_empty()
    }

    fn has_log_event(&self, event: &str) -> bool {
        self.log_requests
            .lock()
            .expect("log requests")
            .iter()
            .flat_map(|request| &request.resource_logs)
            .flat_map(|resource| &resource.scope_logs)
            .flat_map(|scope| &scope.log_records)
            .any(|record| Self::has_attribute(&record.attributes, "event", event))
    }

    fn metric_names(&self) -> std::collections::BTreeSet<String> {
        self.metric_requests
            .lock()
            .expect("metric requests")
            .iter()
            .flat_map(|request| &request.resource_metrics)
            .flat_map(|resource| &resource.scope_metrics)
            .flat_map(|scope| &scope.metrics)
            .map(|metric| metric.name.clone())
            .collect()
    }

    fn assert_metric_attributes_are_bounded(&self) {
        let forbidden = [
            "request_id",
            "trace_id",
            "derivation_path",
            "store_path",
            "shared_build_key",
            "execution_id",
            "allocation_id",
            "quota_subject",
            "endpoint",
            "namespace",
        ];
        let metric_requests = self.metric_requests.lock().expect("metric requests");
        for metric in metric_requests
            .iter()
            .flat_map(|request| &request.resource_metrics)
            .flat_map(|resource| &resource.scope_metrics)
            .flat_map(|scope| &scope.metrics)
        {
            for attribute in Self::metric_attributes(metric) {
                assert!(
                    !forbidden.contains(&attribute.key.as_str()),
                    "metric {} contains forbidden attribute {}",
                    metric.name,
                    attribute.key
                );
            }
        }
    }

    fn metric_attributes(
        metric: &opentelemetry_proto::tonic::metrics::v1::Metric,
    ) -> Box<dyn Iterator<Item = &opentelemetry_proto::tonic::common::v1::KeyValue> + '_> {
        use opentelemetry_proto::tonic::metrics::v1::metric::Data;

        match metric.data.as_ref() {
            Some(Data::Gauge(gauge)) => Box::new(
                gauge
                    .data_points
                    .iter()
                    .flat_map(|point| point.attributes.iter()),
            ),
            Some(Data::Sum(sum)) => Box::new(
                sum.data_points
                    .iter()
                    .flat_map(|point| point.attributes.iter()),
            ),
            Some(Data::Histogram(histogram)) => Box::new(
                histogram
                    .data_points
                    .iter()
                    .flat_map(|point| point.attributes.iter()),
            ),
            _ => Box::new(std::iter::empty()),
        }
    }

    fn assert_correlated(&self, request_id: &str, service_name: &str) -> String {
        let trace_requests = self.trace_requests.lock().expect("trace requests");
        let log_requests = self.log_requests.lock().expect("log requests");
        let metric_requests = self.metric_requests.lock().expect("metric requests");
        let trace = trace_requests
            .iter()
            .flat_map(|request| &request.resource_spans)
            .flat_map(|resource| &resource.scope_spans)
            .flat_map(|scope| &scope.spans)
            .find(|span| span.name == "request")
            .expect("request trace span in encoded OTLP request");
        let log = log_requests
            .iter()
            .flat_map(|request| &request.resource_logs)
            .flat_map(|resource| &resource.scope_logs)
            .flat_map(|scope| &scope.log_records)
            .find(|record| Self::has_attribute(&record.attributes, "request_id", request_id))
            .expect("request log record");
        assert_eq!(trace.trace_id, log.trace_id);
        assert_eq!(trace.span_id, log.span_id);
        assert!(Self::has_attribute(
            &trace.attributes,
            "request_id",
            request_id
        ));
        assert!(!trace.trace_id.is_empty());
        assert!(!trace.span_id.is_empty());
        assert!(metric_requests.iter().all(|request| {
            request.resource_metrics.iter().all(|resource| {
                Self::has_service_name(resource.resource.as_ref(), service_name)
                    && resource.scope_metrics.iter().all(|scope| {
                        scope
                            .metrics
                            .iter()
                            .all(|metric| !Self::metric_has_correlation_identifier(metric))
                    })
            })
        }));
        assert!(trace_requests.iter().all(|request| {
            request
                .resource_spans
                .iter()
                .all(|resource| Self::has_service_name(resource.resource.as_ref(), service_name))
        }));
        assert!(log_requests.iter().all(|request| {
            request
                .resource_logs
                .iter()
                .all(|resource| Self::has_service_name(resource.resource.as_ref(), service_name))
        }));
        trace
            .trace_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn has_attribute(
        attributes: &[opentelemetry_proto::tonic::common::v1::KeyValue],
        key: &str,
        value: &str,
    ) -> bool {
        attributes.iter().any(|attribute| {
            attribute.key == key
                && matches!(
                    attribute.value.as_ref().and_then(|value| value.value.as_ref()),
                    Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(actual)) if actual == value
                )
        })
    }

    fn has_attribute_key(
        attributes: &[opentelemetry_proto::tonic::common::v1::KeyValue],
        key: &str,
    ) -> bool {
        attributes.iter().any(|attribute| attribute.key == key)
    }

    fn has_service_name(
        resource: Option<&opentelemetry_proto::tonic::resource::v1::Resource>,
        service_name: &str,
    ) -> bool {
        resource.is_some_and(|resource| {
            Self::has_attribute(&resource.attributes, "service.name", service_name)
        })
    }

    fn metric_has_correlation_identifier(
        metric: &opentelemetry_proto::tonic::metrics::v1::Metric,
    ) -> bool {
        use opentelemetry_proto::tonic::metrics::v1::metric::Data;

        let mut points: Box<
            dyn Iterator<Item = &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint> + '_,
        > = match metric.data.as_ref() {
            Some(Data::Gauge(gauge)) => Box::new(gauge.data_points.iter()),
            Some(Data::Sum(sum)) => Box::new(sum.data_points.iter()),
            _ => Box::new(std::iter::empty()),
        };
        points.any(|point| {
            Self::has_attribute_key(&point.attributes, "request_id")
                || Self::has_attribute_key(&point.attributes, "trace_id")
                || point
                    .exemplars
                    .iter()
                    .any(|exemplar| !exemplar.trace_id.is_empty() || !exemplar.span_id.is_empty())
        })
    }
}

#[cfg(test)]
#[tonic::async_trait]
impl opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService
    for Collector
{
    async fn export(
        &self,
        request: tonic::Request<
            opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest,
        >,
    ) -> Result<
        tonic::Response<
            opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse,
        >,
        tonic::Status,
    > {
        self.trace_requests
            .lock()
            .expect("trace requests")
            .push(request.into_inner());
        Ok(tonic::Response::new(Default::default()))
    }
}

#[cfg(test)]
#[tonic::async_trait]
impl opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsService
    for Collector
{
    async fn export(
        &self,
        request: tonic::Request<
            opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest,
        >,
    ) -> Result<
        tonic::Response<opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceResponse>,
        tonic::Status,
    > {
        self.log_requests
            .lock()
            .expect("log requests")
            .push(request.into_inner());
        Ok(tonic::Response::new(Default::default()))
    }
}

#[cfg(test)]
#[tonic::async_trait]
impl opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsService
    for Collector
{
    async fn export(
        &self,
        request: tonic::Request<
            opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest,
        >,
    ) -> Result<
        tonic::Response<
            opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceResponse,
        >,
        tonic::Status,
    > {
        self.metric_requests
            .lock()
            .expect("metric requests")
            .push(request.into_inner());
        Ok(tonic::Response::new(Default::default()))
    }
}

#[cfg(test)]
#[derive(Clone)]
struct HttpCollector {
    endpoint: String,
    paths: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
impl HttpCollector {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn has_all_signals(&self) -> bool {
        let paths = self.paths.lock().expect("HTTP collector paths");
        ["/v1/traces", "/v1/logs", "/v1/metrics"]
            .iter()
            .all(|expected| paths.iter().any(|path| path == expected))
    }
}

#[cfg(test)]
fn start_http_collector() -> HttpCollector {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("HTTP collector listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking HTTP collector listener");
    let collector = HttpCollector {
        endpoint: format!(
            "http://{}",
            listener.local_addr().expect("HTTP collector address")
        ),
        paths: Default::default(),
    };
    let paths = std::sync::Arc::clone(&collector.paths);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    break None;
                };
                if read == 0 {
                    break None;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    break Some(position + 4);
                }
                if request.len() > 64 * 1024 {
                    break None;
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let path = headers
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len().saturating_sub(header_end) < content_length {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            paths.lock().expect("HTTP collector paths").push(path);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    collector
}

#[cfg(test)]
fn start_collector() -> Collector {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("collector listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("collector address")
    );
    listener
        .set_nonblocking(true)
        .expect("nonblocking collector listener");
    let collector = Collector {
        endpoint,
        trace_requests: Default::default(),
        log_requests: Default::default(),
        metric_requests: Default::default(),
    };
    let service = collector.clone();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().expect("collector runtime").block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("Tokio collector listener");
            tonic::transport::Server::builder()
                .add_service(opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer::new(service.clone()))
                .add_service(opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsServiceServer::new(service.clone()))
                .add_service(opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .expect("collector server");
        });
    });
    collector
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{Collector, SERVICE_NAME, start_collector, start_http_collector, telemetry_tests};

    #[test]
    fn emits_service_identity() {
        if std::env::var_os("TELCHAR_TELEMETRY_IDENTITY_TEST").is_none() {
            return;
        }
        let telemetry = super::Telemetry::initialize("telchar-nomad-worker", "identity-version")
            .expect("telemetry initializes");
        {
            let span = tracing::info_span!("request", request_id = "identity-test");
            let _entered = span.enter();
            tracing::info!(request_id = "identity-test", "service started");
            opentelemetry::global::meter("identity-test")
                .u64_counter("test.starts")
                .build()
                .add(1, &[]);
        }
        telemetry.shutdown();
    }

    #[test]
    fn exports_configured_service_identity() {
        let collector = start_collector();
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "telemetry::tests::emits_service_identity",
                "--exact",
                "--nocapture",
            ])
            .env("TELCHAR_TELEMETRY_IDENTITY_TEST", "1")
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
            .env("RUST_LOG", "info")
            .output()
            .expect("identity process starts");
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        assert!(collector.has_all_signals());
        collector.assert_correlated("identity-test", "telchar-nomad-worker");
        for request in collector.log_requests.lock().expect("log requests").iter() {
            for resource in &request.resource_logs {
                assert!(Collector::has_attribute(
                    &resource.resource.as_ref().expect("resource").attributes,
                    "service.version",
                    "identity-version"
                ));
            }
        }
    }

    #[test]
    fn exports_set_options_event_to_otlp() {
        let _guard = telemetry_tests()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let collector = start_collector();
        let current_executable = std::env::current_exe().expect("current test executable");
        let test_directory = current_executable
            .parent()
            .expect("test executable directory")
            .to_owned();
        let telchar_binary = std::env::var_os("CARGO_BIN_EXE_telchar")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                test_directory
                    .parent()
                    .expect("target profile directory")
                    .join("telchar")
            });
        let operation_dispatch = std::fs::read_dir(test_directory)
            .expect("test binary directory reads")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("operation_dispatch-"))
                    && entry.path().is_file()
            })
            .filter_map(|entry| {
                let output = Command::new(entry.path())
                    .args(["--list", "--format", "terse"])
                    .output()
                    .ok()?;
                let tests = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|line| line.ends_with(": test"))
                    .count();
                (output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(
                        "protocol::live_set_options_request_returns_terminal_frame: test",
                    ))
                .then_some((tests, entry))
            })
            .max_by_key(|(tests, entry)| {
                (
                    *tests,
                    entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .ok(),
                )
            })
            .map(|(_, entry)| entry)
            .expect("current operation_dispatch test binary");
        let status = Command::new(operation_dispatch.path())
            .arg("protocol::live_set_options_request_returns_terminal_frame")
            .env("CARGO_BIN_EXE_telchar", &telchar_binary)
            .env("TELCHAR_TEST_OTLP_ENDPOINT", collector.endpoint())
            .status()
            .expect("SetOptions test process starts");
        assert!(status.success(), "SetOptions test process failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !collector.has_log_event("worker.set_options.completed")
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            collector.has_log_event("worker.set_options.completed"),
            "worker SetOptions event was not exported through OTLP"
        );
    }

    fn run_smoke(endpoint: String, protocol: &str) -> std::process::Output {
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .args(["run", "--quiet", "--bin", "telchar", "--locked"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", protocol)
            .env("TELCHAR_SMOKE_REQUEST_ID", "request-smoke-001")
            .env("TELCHAR_SMOKE_ERROR", "1")
            .env("TELCHAR_SMOKE_OPERATIONAL_METRICS", "1")
            .output()
            .expect("Telchar process starts")
    }

    fn assert_local_smoke_output(output: &std::process::Output) -> String {
        assert!(
            output.status.success(),
            "Telchar process failed: {output:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Nix worker protocol\n"),
            "missing protocol output: {output:?}"
        );
        assert!(
            !stdout.contains(" trace_id="),
            "local telemetry contaminated command output: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(" trace_id="),
            "missing local trace ID: {output:?}"
        );
        assert!(
            stderr.contains(" INFO request started"),
            "missing local log: {output:?}"
        );
        assert!(
            stderr.contains(" ERROR smoke error"),
            "missing local error: {output:?}"
        );

        stderr.into_owned()
    }

    fn assert_grpc_signals(collector: &Collector, stderr: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !collector.has_all_signals() {
            std::thread::sleep(Duration::from_millis(10));
        }

        let trace_id = collector.assert_correlated("request-smoke-001", SERVICE_NAME);
        assert!(stderr.contains(&format!("trace_id={trace_id}")));
        let names = collector.metric_names();
        for expected in [
            "telchar.build.requests",
            "telchar.build.request.duration",
            "telchar.service.session.rejections",
            "telchar.shared_build.queue.depth",
            "telchar.shared_build.follower.wait.duration",
            "telchar.backend.permits.active",
            "telchar.backend.permits.waiting",
            "telchar.backend.permit.wait.duration",
            "telchar.static_ssh.health.checks",
            "telchar.static_ssh.health.check.duration",
            "telchar.configuration.reload.duration",
            "telchar.configuration.reload.static_ssh.added",
            "telchar.configuration.reload.static_ssh.removed",
            "telchar.cache.substitutions",
            "telchar.transfer.active",
            "telchar.transfer.bytes",
            "telchar.transfer.failures",
            "telchar.recovery.attempts",
            "telchar.recovery.duration",
            "telchar.recovery.outcomes",
            "telchar.recovery.monitoring",
            "telchar.nomad.pending",
        ] {
            assert!(
                names.contains(expected),
                "missing OTLP metric {expected}: {names:?}"
            );
        }
        collector.assert_metric_attributes_are_bounded();
    }

    #[test]
    fn exports_otlp_signals_before_application_work() {
        let _guard = telemetry_tests()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let collector = start_collector();
        let output = run_smoke(collector.endpoint(), "grpc");
        let stderr = assert_local_smoke_output(&output);
        assert_grpc_signals(&collector, &stderr);
    }

    #[test]
    fn exports_otlp_signals_over_http_protobuf() {
        let _guard = telemetry_tests()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let collector = start_http_collector();
        let output = run_smoke(collector.endpoint(), "http/protobuf");
        assert_local_smoke_output(&output);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !collector.has_all_signals() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            collector.has_all_signals(),
            "HTTP collector missed OTLP signals: {:?}",
            collector.paths.lock().expect("HTTP collector paths")
        );
    }
}
