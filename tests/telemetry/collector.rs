//! Receives and inspects encoded OTLP signals over gRPC.

#[derive(Clone)]
pub struct Collector {
    endpoint: String,
    trace_requests: std::sync::Arc<
        std::sync::Mutex<
            Vec<opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest>,
        >,
    >,
    pub log_requests: std::sync::Arc<
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

impl Collector {
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    pub fn has_all_signals(&self) -> bool {
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

    pub fn has_log_event(&self, event: &str) -> bool {
        self.log_requests
            .lock()
            .expect("log requests")
            .iter()
            .flat_map(|request| &request.resource_logs)
            .flat_map(|resource| &resource.scope_logs)
            .flat_map(|scope| &scope.log_records)
            .any(|record| Self::has_attribute(&record.attributes, "event", event))
    }

    pub fn assert_span_parent(&self, parent_name: &str, child_name: &str) {
        let trace_requests = self.trace_requests.lock().expect("trace requests");
        let spans = trace_requests
            .iter()
            .flat_map(|request| &request.resource_spans)
            .flat_map(|resource| &resource.scope_spans)
            .flat_map(|scope| &scope.spans)
            .collect::<Vec<_>>();
        let parent = spans
            .iter()
            .find(|span| span.name == parent_name)
            .unwrap_or_else(|| panic!("missing parent span {parent_name}"));
        let child = spans
            .iter()
            .find(|span| span.name == child_name)
            .unwrap_or_else(|| panic!("missing child span {child_name}"));
        assert_eq!(parent.trace_id, child.trace_id);
        assert_eq!(parent.span_id, child.parent_span_id);
    }

    pub fn metric_names(&self) -> std::collections::BTreeSet<String> {
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

    pub fn assert_metric_attributes_are_bounded(&self) {
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

    pub fn assert_correlated(&self, request_id: &str, service_name: &str) -> String {
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

    pub fn has_attribute(
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

    pub fn has_service_name(
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

pub fn start_collector() -> Collector {
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
