use std::error::Error;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{
    BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{
    BatchConfigBuilder as TraceBatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider,
};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

const SERVICE_NAME: &str = "telchar";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(1);
const EXPORT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_QUEUE_SIZE: usize = 256;
const MAX_EXPORT_BATCH_SIZE: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Telemetry {
    logger_provider: SdkLoggerProvider,
    meter_provider: SdkMeterProvider,
    tracer_provider: SdkTracerProvider,
    runtime: tokio::runtime::Runtime,
}

impl Telemetry {
    pub fn initialize(local_format: bool) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:4317".to_owned());
        Self::initialize_with_endpoint(local_format, endpoint)
    }

    fn initialize_with_endpoint(
        local_format: bool,
        endpoint: String,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let resource = Resource::builder()
            .with_service_name(SERVICE_NAME)
            .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))])
            .build();

        let tracer_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_span_processor(
                        BatchSpanProcessor::builder(exporter)
                            .with_batch_config(
                                TraceBatchConfigBuilder::default()
                                    .with_max_queue_size(MAX_QUEUE_SIZE)
                                    .with_max_export_batch_size(MAX_EXPORT_BATCH_SIZE)
                                    .with_scheduled_delay(EXPORT_INTERVAL)
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
        })?;
        let meter_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkMeterProvider::builder()
                    .with_resource(resource.clone())
                    .with_reader(
                        PeriodicReader::builder(exporter)
                            .with_interval(EXPORT_INTERVAL)
                            .build(),
                    )
                    .build(),
            )
        })?;
        let logger_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkLoggerProvider::builder()
                    .with_resource(resource)
                    .with_log_processor(
                        BatchLogProcessor::builder(exporter)
                            .with_batch_config(
                                LogBatchConfigBuilder::default()
                                    .with_max_queue_size(MAX_QUEUE_SIZE)
                                    .with_max_export_batch_size(MAX_EXPORT_BATCH_SIZE)
                                    .with_scheduled_delay(EXPORT_INTERVAL)
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
        })?;

        global::set_tracer_provider(tracer_provider.clone());
        global::set_meter_provider(meter_provider.clone());

        let registry = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer(SERVICE_NAME)))
            .with(OpenTelemetryTracingBridge::new(&logger_provider));
        if local_format {
            registry.with(tracing_subscriber::fmt::layer()).try_init()?;
        } else {
            registry.try_init()?;
        }

        Ok(Self {
            logger_provider,
            meter_provider,
            tracer_provider,
            runtime,
        })
    }

    pub fn shutdown(self) {
        let (completed, result) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = self.logger_provider.shutdown_with_timeout(EXPORT_TIMEOUT);
            let _ = self.meter_provider.shutdown_with_timeout(EXPORT_TIMEOUT);
            let _ = self.tracer_provider.shutdown_with_timeout(EXPORT_TIMEOUT);
            self.runtime.shutdown_timeout(EXPORT_TIMEOUT);
            let _ = completed.send(());
        });
        let _ = result.recv_timeout(SHUTDOWN_TIMEOUT);
    }
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

    fn assert_correlated(&self, request_id: &str, service_name: &str) {
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

    use super::{SERVICE_NAME, start_collector};

    #[test]
    fn exports_otlp_signals_before_application_work() {
        let collector = start_collector();
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .args(["run", "--quiet", "--bin", "telchar", "--locked"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
            .env("TELCHAR_SMOKE_REQUEST_ID", "request-smoke-001")
            .output()
            .expect("Telchar process starts");
        assert!(
            output.status.success(),
            "Telchar process failed: {output:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Nix worker protocol\n"
        );
        assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !collector.has_all_signals() {
            std::thread::sleep(Duration::from_millis(10));
        }

        collector.assert_correlated("request-smoke-001", SERVICE_NAME);
    }
}
