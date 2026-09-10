use std::collections::BTreeMap;
use std::io;

use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator as _};
use opentelemetry::trace::{TraceContextExt as _, TraceId};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

pub const MAXIMUM_TRACEPARENT_BYTES: usize = 55;
pub const MAXIMUM_TRACESTATE_BYTES: usize = 512;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TraceContext {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl TraceContext {
    pub fn capture_current() -> Self {
        let mut fields = BTreeMap::new();
        TraceContextPropagator::new().inject_context(
            &tracing::Span::current().context(),
            &mut MapInjector(&mut fields),
        );
        Self {
            traceparent: fields
                .remove("traceparent")
                .filter(|value| !value.is_empty()),
            tracestate: fields
                .remove("tracestate")
                .filter(|value| !value.is_empty()),
        }
    }

    pub fn new(traceparent: Option<String>, tracestate: Option<String>) -> io::Result<Self> {
        let context = Self {
            traceparent,
            tracestate,
        };
        context.validate()?;
        Ok(context)
    }

    pub fn traceparent(&self) -> Option<&str> {
        self.traceparent.as_deref()
    }

    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }

    pub fn trace_id(&self) -> Option<TraceId> {
        let context = self.extract();
        context
            .span()
            .span_context()
            .is_valid()
            .then(|| context.span().span_context().trace_id())
    }

    pub fn set_parent(&self, span: &tracing::Span) {
        if self.traceparent.is_some() {
            let _ = span.set_parent(self.extract());
        }
    }

    pub fn add_link(&self, span: &tracing::Span) {
        let context = self.extract();
        let span_context = context.span();
        if span_context.span_context().is_valid() {
            span.add_link(span_context.span_context().clone());
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if self
            .traceparent
            .as_ref()
            .is_some_and(|value| value.len() > MAXIMUM_TRACEPARENT_BYTES || value.contains('\0'))
            || self
                .tracestate
                .as_ref()
                .is_some_and(|value| value.len() > MAXIMUM_TRACESTATE_BYTES || value.contains('\0'))
            || self
                .traceparent
                .as_ref()
                .is_some_and(|value| value.is_empty())
            || self
                .tracestate
                .as_ref()
                .is_some_and(|value| value.is_empty())
            || self.traceparent.is_none() && self.tracestate.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trace context is invalid",
            ));
        }
        if self.traceparent.is_some() && self.trace_id().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trace context is invalid",
            ));
        }
        Ok(())
    }

    fn extract(&self) -> Context {
        TraceContextPropagator::new().extract(&TraceContextExtractor(self))
    }
}

struct MapInjector<'a>(&'a mut BTreeMap<String, String>);

impl Injector for MapInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
}

struct TraceContextExtractor<'a>(&'a TraceContext);

impl Extractor for TraceContextExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "traceparent" => self.0.traceparent(),
            "tracestate" => self.0.tracestate(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::with_capacity(2);
        if self.0.traceparent.is_some() {
            keys.push("traceparent");
        }
        if self.0.tracestate.is_some() {
            keys.push("tracestate");
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_w3c_trace_context() {
        let context = TraceContext::new(
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned()),
            Some("vendor=value".to_owned()),
        )
        .expect("trace context validates");

        assert_eq!(
            context.trace_id().expect("trace ID exists").to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
    }

    #[test]
    fn rejects_invalid_trace_context() {
        assert!(TraceContext::new(Some("invalid".to_owned()), None).is_err());
        assert!(TraceContext::new(None, Some("vendor=value".to_owned())).is_err());
        assert!(
            TraceContext::new(
                Some(format!(
                    "00-{}-00f067aa0ba902b7-01",
                    "4".repeat(MAXIMUM_TRACEPARENT_BYTES)
                )),
                None,
            )
            .is_err()
        );
        assert!(
            TraceContext::new(
                Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned()),
                Some("v".repeat(MAXIMUM_TRACESTATE_BYTES + 1)),
            )
            .is_err()
        );
    }

    #[test]
    fn absent_trace_context_can_be_linked() {
        TraceContext::default().add_link(&tracing::Span::none());
    }
}
