//! Initializes bounded structured logs and OTLP exporters without retaining sensitive request data.

mod progress;
pub use progress::Progress;

use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{
    BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{
    BatchConfigBuilder as TraceBatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider,
};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

const EXPORT_TIMEOUT: Duration = Duration::from_secs(1);
const EXPORT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_QUEUE_SIZE: usize = 256;
const MAX_EXPORT_BATCH_SIZE: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

fn http_client() -> Result<otlp_http_client::blocking::Client, otlp_http_client::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    otlp_http_client::blocking::Client::builder()
        .timeout(EXPORT_TIMEOUT)
        .build()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OtlpTransport {
    Grpc,
    HttpProtobuf,
}

impl OtlpTransport {
    fn endpoint(self, base: &str, signal: &str) -> String {
        match self {
            Self::Grpc => base.to_owned(),
            Self::HttpProtobuf => format!("{}/v1/{signal}", base.trim_end_matches('/')),
        }
    }

    fn from_environment() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::from_environment_value(std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").ok().as_deref())
    }

    fn from_environment_value(value: Option<&str>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        match value.unwrap_or("grpc") {
            "grpc" => Ok(Self::Grpc),
            "http/protobuf" => Ok(Self::HttpProtobuf),
            _ => Err("unsupported OTLP transport protocol".into()),
        }
    }
}

struct LocalFormat;

struct LocalWriter;

enum LocalOutput {
    StandardError(std::io::Stderr),
}

impl std::io::Write for LocalOutput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::StandardError(output) => output.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::StandardError(output) => output.flush(),
        }
    }
}

impl<'a> MakeWriter<'a> for LocalWriter {
    type Writer = LocalOutput;

    fn make_writer(&'a self) -> Self::Writer {
        LocalOutput::StandardError(std::io::stderr())
    }
}

impl<S, N> FormatEvent<S, N> for LocalFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| fmt::Error)?
            .as_secs();
        let trace_id = tracing::Span::current()
            .context()
            .span()
            .span_context()
            .trace_id()
            .to_string();
        let trace_id = if trace_id == "00000000000000000000000000000000" {
            "none".to_owned()
        } else {
            trace_id
        };
        write!(
            writer,
            "{time} trace_id={trace_id} {} ",
            event.metadata().level()
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

pub struct Telemetry {
    logger_provider: SdkLoggerProvider,
    meter_provider: SdkMeterProvider,
    tracer_provider: SdkTracerProvider,
    runtime: tokio::runtime::Runtime,
}

impl Telemetry {
    pub fn initialize(
        service_name: &'static str,
        service_version: &'static str,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let transport = OtlpTransport::from_environment()?;
        let endpoint =
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_else(|_| match transport {
                OtlpTransport::Grpc => "http://127.0.0.1:4317".to_owned(),
                OtlpTransport::HttpProtobuf => "http://127.0.0.1:4318".to_owned(),
            });
        Self::initialize_with_endpoint(endpoint, transport, service_name, service_version)
    }

    fn initialize_with_endpoint(
        endpoint: String,
        transport: OtlpTransport,
        service_name: &'static str,
        service_version: &'static str,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let resource = Resource::builder()
            .with_service_name(service_name)
            .with_attributes([KeyValue::new("service.version", service_version)])
            .build();

        let trace_http_client = (transport == OtlpTransport::HttpProtobuf)
            .then(http_client)
            .transpose()?;
        let tracer_provider = runtime.block_on(async {
            let exporter = match transport {
                OtlpTransport::Grpc => opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint.clone())
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
                OtlpTransport::HttpProtobuf => opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_http_client(
                        trace_http_client.expect("HTTP client exists for HTTP transport"),
                    )
                    .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                    .with_endpoint(transport.endpoint(&endpoint, "traces"))
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
            };
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
        let metric_http_client = (transport == OtlpTransport::HttpProtobuf)
            .then(http_client)
            .transpose()?;
        let meter_provider = runtime.block_on(async {
            let exporter = match transport {
                OtlpTransport::Grpc => opentelemetry_otlp::MetricExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint.clone())
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
                OtlpTransport::HttpProtobuf => opentelemetry_otlp::MetricExporter::builder()
                    .with_http()
                    .with_http_client(
                        metric_http_client.expect("HTTP client exists for HTTP transport"),
                    )
                    .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                    .with_endpoint(transport.endpoint(&endpoint, "metrics"))
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
            };
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
        let log_http_client = (transport == OtlpTransport::HttpProtobuf)
            .then(http_client)
            .transpose()?;
        let logger_provider = runtime.block_on(async {
            let exporter = match transport {
                OtlpTransport::Grpc => opentelemetry_otlp::LogExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
                OtlpTransport::HttpProtobuf => opentelemetry_otlp::LogExporter::builder()
                    .with_http()
                    .with_http_client(
                        log_http_client.expect("HTTP client exists for HTTP transport"),
                    )
                    .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                    .with_endpoint(transport.endpoint(&endpoint, "logs"))
                    .with_timeout(EXPORT_TIMEOUT)
                    .build()?,
            };
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

        tracing_subscriber::registry()
            .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
            .with(tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer(service_name)))
            .with(OpenTelemetryTracingBridge::new(&logger_provider))
            .with(
                tracing_subscriber::fmt::layer()
                    .event_format(LocalFormat)
                    .with_writer(LocalWriter),
            )
            .try_init()?;

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
mod tests {
    use super::OtlpTransport;
    #[test]
    fn selects_supported_otlp_transports() {
        assert_eq!(
            OtlpTransport::from_environment_value(None).expect("default transport"),
            OtlpTransport::Grpc
        );
        assert_eq!(
            OtlpTransport::from_environment_value(Some("grpc")).expect("gRPC transport"),
            OtlpTransport::Grpc
        );
        assert_eq!(
            OtlpTransport::from_environment_value(Some("http/protobuf")).expect("HTTP transport"),
            OtlpTransport::HttpProtobuf
        );
        assert!(OtlpTransport::from_environment_value(Some("http/json")).is_err());
        assert!(OtlpTransport::from_environment_value(Some("prometheus")).is_err());
    }
}
