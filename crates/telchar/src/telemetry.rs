use std::error::Error;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

const SERVICE_NAME: &str = "telchar";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Telemetry {
    logger_provider: SdkLoggerProvider,
    meter_provider: SdkMeterProvider,
    tracer_provider: SdkTracerProvider,
    runtime: tokio::runtime::Runtime,
}

impl Telemetry {
    pub fn initialize(local_format: bool) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let resource = Resource::builder()
            .with_service_name(SERVICE_NAME)
            .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))])
            .build();

        let tracer_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_batch_exporter(exporter)
                    .build(),
            )
        })?;
        let meter_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkMeterProvider::builder()
                    .with_resource(resource.clone())
                    .with_periodic_exporter(exporter)
                    .build(),
            )
        })?;
        let logger_provider = runtime.block_on(async {
            let exporter = opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_timeout(EXPORT_TIMEOUT)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkLoggerProvider::builder()
                    .with_resource(resource)
                    .with_batch_exporter(exporter)
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
        let _ = self.logger_provider.shutdown();
        let _ = self.meter_provider.shutdown();
        let _ = self.tracer_provider.shutdown();
        self.runtime.shutdown_timeout(EXPORT_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use super::Telemetry;

    #[test]
    fn initializes_before_application_event_and_flushes_on_shutdown() {
        let telemetry = Telemetry::initialize(false).expect("telemetry initializes");
        tracing::info!(event = "application.started", "application started");
        telemetry.shutdown();
    }
}
