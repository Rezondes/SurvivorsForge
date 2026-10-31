//! Logs, and with `OTEL_EXPORTER_OTLP_ENDPOINT` set also OpenTelemetry traces (to Tempo), for the
//! API, the worker and the website. A request's trace goes along to the API as `traceparent`, and
//! every span carries its `trace_id`, so Grafana jumps from a log line to its trace.

use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Keeps the exporter running; dropping it sends the last spans.
pub struct Guard(Option<SdkTracerProvider>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(provider) = self.0.take() {
            let _ = provider.shutdown();
        }
    }
}

/// Starts logging as `service` (`api`, `worker`, `web`). Without `OTEL_EXPORTER_OTLP_ENDPOINT` only
/// logs are written, as before.
pub fn init(service: &str) -> Guard {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().filter(|e| !e.trim().is_empty());
    let provider = endpoint.and_then(|_| {
        let exporter = opentelemetry_otlp::SpanExporter::builder().with_http().build().map_err(|e| eprintln!("no trace export: {e}")).ok()?;
        let resource = opentelemetry_sdk::Resource::builder().with_service_name(format!("forge-{service}")).build();
        Some(SdkTracerProvider::builder().with_batch_exporter(exporter).with_resource(resource).build())
    });
    let otel = provider.as_ref().map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("forge")));
    tracing_subscriber::registry().with(filter).with(tracing_subscriber::fmt::layer()).with(otel).init();
    Guard(provider)
}

struct Headers<'a>(&'a http::HeaderMap);

impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

struct HeadersMut<'a>(&'a mut http::HeaderMap);

impl Injector for HeadersMut<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (http::HeaderName::from_bytes(key.as_bytes()), http::HeaderValue::from_str(&value)) {
            self.0.insert(name, value);
        }
    }
}

/// Continues the trace of an incoming `traceparent` in `span`, and records its `trace_id`.
pub fn continue_trace(span: &tracing::Span, headers: &http::HeaderMap) {
    let parent = TraceContextPropagator::new().extract(&Headers(headers));
    if parent.span().span_context().is_valid() {
        let _ = span.set_parent(parent);
    }
    record_trace_id(span);
}

/// Writes the span's trace ID into its `trace_id` field, which the logs then show.
pub fn record_trace_id(span: &tracing::Span) {
    let id = span.context().span().span_context().trace_id();
    if id != opentelemetry::TraceId::INVALID {
        span.record("trace_id", tracing::field::display(id));
    }
}

/// The `traceparent` of the current span, for a request to another service.
pub fn inject(headers: &mut http::HeaderMap) {
    let context = tracing::Span::current().context();
    TraceContextPropagator::new().inject_context(&context, &mut HeadersMut(headers));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_continue_the_incoming_trace() {
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
        let subscriber = tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let mut incoming = http::HeaderMap::new();
        incoming.insert("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".parse().unwrap());
        let outgoing = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", trace_id = tracing::field::Empty);
            continue_trace(&span, &incoming);
            let _entered = span.enter();
            let mut out = http::HeaderMap::new();
            inject(&mut out);
            out
        });
        let sent = outgoing["traceparent"].to_str().unwrap();
        assert!(sent.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"), "same trace, own span: {sent}");
        assert!(!sent.contains("00f067aa0ba902b7"), "the API's parent is this span, not the caller's");
        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans[0].span_context.trace_id().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
    }

    #[test]
    fn traceparent_round_trip() {
        let mut headers = http::HeaderMap::new();
        headers.insert("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".parse().unwrap());
        let parent = TraceContextPropagator::new().extract(&Headers(&headers));
        assert_eq!(parent.span().span_context().trace_id().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        let mut out = http::HeaderMap::new();
        TraceContextPropagator::new().inject_context(&parent, &mut HeadersMut(&mut out));
        assert_eq!(out["traceparent"], "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
    }
}
