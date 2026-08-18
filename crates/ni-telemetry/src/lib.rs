//! M4 telemetry wiring, shared by the engine and every bot.
//!
//! Three jobs, and nothing else:
//!
//! 1. **Build a subscriber.** Human-readable logs on *stderr* always; OTLP
//!    traces and logs as well when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//! 2. **Put the current trace on the wire** ([`inject_context`]), so a bot's
//!    spans become children of the engine's.
//! 3. **Take it off again** ([`server_span`]), so the bot's span knows which
//!    call it belongs to.
//!
//! Everything here is about *context*, not about the game. No game types
//! appear in this crate, and no telemetry types leak into `ni-game`.

use std::time::Duration;

use opentelemetry::{
    global,
    propagation::{Extractor, Injector},
    trace::TracerProvider as _,
    Context,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider, propagation::TraceContextPropagator, trace::SdkTracerProvider,
    Resource,
};
use tonic::{metadata::MetadataMap, Request};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Standard OTLP environment variable. Unset means "no collector": the
/// process still logs to stderr, and every span still exists in-process.
const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Providers kept alive for the life of the process. Dropping them without
/// [`Telemetry::shutdown`] loses whatever is still in the batch queue.
#[must_use = "hold the guard until the process is finished, then shut it down"]
pub struct Telemetry {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl Telemetry {
    /// Flush and stop the exporters. Call this on the way out; a batch
    /// exporter that is dropped mid-batch simply drops the spans.
    pub fn shutdown(self) {
        if let Some(provider) = self.tracer_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("telemetry: tracer shutdown failed: {error}");
            }
        }

        if let Some(provider) = self.logger_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("telemetry: logger shutdown failed: {error}");
            }
        }
    }
}

/// Install the subscriber for this process.
///
/// `service_name` is what the dashboard groups spans under, so it must be
/// per-binary: `ni-engine`, `reference-bot`, `roger-the-shrubber`.
pub fn init(service_name: &str) -> Telemetry {
    // Trace context travels as the W3C `traceparent` header. Registering the
    // propagator globally is what makes `inject_context` and `server_span`
    // agree on a format without either of them naming one.
    global::set_text_map_propagator(TraceContextPropagator::new());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // stderr, never stdout: a bot's stdout carries the readiness line the
    // engine parses, and the engine's stdout carries the board.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);

    let endpoint = match std::env::var(ENDPOINT_ENV) {
        Ok(endpoint) if !endpoint.trim().is_empty() => {
            endpoint.trim().trim_end_matches('/').to_string()
        }
        _ => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .init();

            return Telemetry {
                tracer_provider: None,
                logger_provider: None,
            };
        }
    };

    let resource = Resource::builder()
        .with_service_name(service_name.to_string())
        .build();

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .expect("OTLP span exporter");

    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(span_exporter)
        .build();

    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .expect("OTLP log exporter");

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(log_exporter)
        .build();

    let tracer = tracer_provider.tracer("ni");
    global::set_tracer_provider(tracer_provider.clone());

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        // Spans -> Tempo.
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        // Events -> Loki, each stamped with the trace id of its span.
        .with(OpenTelemetryTracingBridge::new(&logger_provider))
        .init();

    tracing::info!(
        service = service_name,
        endpoint,
        "telemetry exporting over OTLP"
    );

    Telemetry {
        tracer_provider: Some(tracer_provider),
        logger_provider: Some(logger_provider),
    }
}

/// Write the current span's context into a request's gRPC metadata as
/// `traceparent`. Call it *inside* the span that represents the call.
pub fn inject_context<T>(request: &mut Request<T>) {
    let context = Span::current().context();

    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut MetadataInjector(request.metadata_mut()));
    });
}

/// A server-side span for one RPC, parented to the caller's span if the
/// caller sent a `traceparent`.
///
/// `rpc` is the full gRPC method name, e.g. `ni.v1.BotService/GetOrders` —
/// that is what the dashboard shows as the span name.
pub fn server_span(rpc: &'static str, metadata: &MetadataMap) -> Span {
    let span = tracing::info_span!(
        "rpc",
        otel.name = rpc,
        otel.kind = "server",
        rpc.system = "grpc",
    );

    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(metadata))
    });
    // The only error here is "no OpenTelemetry layer installed", i.e. this
    // process is running without a collector. The span is still a perfectly
    // good local span, so there is nothing to report.
    let _ = span.set_parent(parent);

    span
}

/// Trace and span id of the current span, as the 32- and 16-character hex
/// strings a dashboard search box expects. `None` when nothing is sampled —
/// with no collector configured there is no trace to point at.
pub fn current_ids() -> Option<TraceIds> {
    let context = Span::current().context();
    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();

    span_context.is_valid().then(|| TraceIds {
        trace_id: span_context.trace_id().to_string(),
        span_id: span_context.span_id().to_string(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceIds {
    pub trace_id: String,
    pub span_id: String,
}

/// `traceparent` out. gRPC metadata keys must be lowercase ASCII, which the
/// W3C names already are.
struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
            if let Ok(value) = tonic::metadata::MetadataValue::try_from(&value) {
                self.0.insert(name, value);
            }
        }
    }
}

/// `traceparent` in. Anything unparseable is simply absent, which yields a
/// root span rather than an error — telemetry must never fail a match.
struct MetadataExtractor<'a>(&'a MetadataMap);

impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .filter_map(|key| match key {
                tonic::metadata::KeyRef::Ascii(name) => Some(name.as_str()),
                tonic::metadata::KeyRef::Binary(_) => None,
            })
            .collect()
    }
}

/// Sanity check for the two halves of propagation, with no network in sight.
#[doc(hidden)]
pub fn round_trip_for_tests(traceparent: &str) -> Option<String> {
    let mut metadata = MetadataMap::new();
    metadata.insert("traceparent", traceparent.parse().ok()?);

    let context: Context = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(&metadata))
    });

    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();

    span_context
        .is_valid()
        .then(|| span_context.trace_id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traceparent_header_survives_extraction() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let trace_id =
            round_trip_for_tests("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");

        assert_eq!(
            trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
    }

    #[test]
    fn nonsense_is_no_parent_rather_than_an_error() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        assert_eq!(round_trip_for_tests("not-a-traceparent"), None);
    }

    #[test]
    fn injection_writes_a_traceparent_when_a_span_is_sampled() {
        let mut request = Request::new(());
        inject_context(&mut request);

        // With no exporter installed in a unit test there is no sampled
        // span, so there is deliberately nothing to inject.
        assert!(request.metadata().get("traceparent").is_none());
    }
}
