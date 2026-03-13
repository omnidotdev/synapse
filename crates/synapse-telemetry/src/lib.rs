//! Telemetry for Synapse
//!
//! Provides OpenTelemetry metrics, tracing, and logging via the `tracing` ecosystem

mod metadata;
pub mod metrics;

use std::time::Duration;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use synapse_config::TelemetryConfig;

// Re-export common OpenTelemetry types for metrics
pub use opentelemetry::{
    KeyValue,
    metrics::{Counter, Gauge, Histogram, Meter},
};

/// Guard that ensures proper cleanup of telemetry resources on drop
pub struct TelemetryGuard {
    meter_provider: Option<SdkMeterProvider>,
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl TelemetryGuard {
    /// Force flush all pending metrics immediately
    ///
    /// # Errors
    ///
    /// Returns an error if the meter provider fails to flush
    pub fn force_flush(&self) -> anyhow::Result<()> {
        if let Some(ref provider) = self.meter_provider {
            provider
                .force_flush()
                .map_err(|e| anyhow::anyhow!("failed to flush metrics: {e}"))?;
        }
        Ok(())
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.meter_provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("failed to shutdown meter provider: {e}");
        }
        if let Some(provider) = self.tracer_provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("failed to shutdown tracer provider: {e}");
        }
    }
}

/// Initialize telemetry from configuration
///
/// Sets up the `tracing-subscriber` with optional OTLP export for traces,
/// metrics, and logs. Returns a guard that must be held for the lifetime
/// of the application.
///
/// # Errors
///
/// Returns an error if OTLP exporter initialization fails for metrics or tracing
pub fn init(config: Option<&TelemetryConfig>, log_filter: &str) -> anyhow::Result<TelemetryGuard> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_new(log_filter).unwrap_or_else(|_| EnvFilter::new("info"));

    let mut guard = TelemetryGuard {
        meter_provider: None,
        tracer_provider: None,
    };

    match config {
        Some(telemetry_config) if has_exporter(telemetry_config) => {
            let resource = metadata::build_resource(telemetry_config);

            // Set up metrics
            let meter_provider = init_metrics(telemetry_config, resource.clone())?;
            global::set_meter_provider(meter_provider.clone());
            guard.meter_provider = Some(meter_provider);

            // Set up tracing with OTLP export
            let tracer_provider = init_tracer(telemetry_config, resource)?;
            let tracer = tracer_provider.tracer("synapse");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            global::set_tracer_provider(tracer_provider.clone());
            guard.tracer_provider = Some(tracer_provider);

            let fmt_layer = tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false);

            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(otel_layer)
                .init();
        }
        _ => {
            // No telemetry config — just set up fmt logging
            let fmt_layer = tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false);

            tracing_subscriber::registry().with(filter).with(fmt_layer).init();
        }
    }

    Ok(guard)
}

/// Check if any exporter is configured
fn has_exporter(config: &TelemetryConfig) -> bool {
    config.exporter.is_some()
        || config.tracing.as_ref().is_some_and(|t| t.exporter.is_some())
        || config.metrics.as_ref().is_some_and(|m| m.exporter.is_some())
        || config.logs.as_ref().is_some_and(|l| l.exporter.is_some())
}

/// Initialize OTLP metrics export
fn init_metrics(config: &TelemetryConfig, resource: opentelemetry_sdk::Resource) -> anyhow::Result<SdkMeterProvider> {
    use opentelemetry_sdk::metrics::PeriodicReader;

    let exporter_config = config
        .metrics
        .as_ref()
        .and_then(|m| m.exporter.as_ref())
        .or(config.exporter.as_ref())
        .ok_or_else(|| anyhow::anyhow!("no metrics exporter configured"))?;

    let exporter = build_metrics_exporter(exporter_config)?;

    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(
            exporter_config.batch.as_ref().map_or(30, |b| b.scheduled_delay),
        ))
        .build();

    let provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build();

    Ok(provider)
}

/// Build OTLP metrics exporter based on protocol
fn build_metrics_exporter(
    config: &synapse_config::telemetry::exporters::ExporterConfig,
) -> anyhow::Result<opentelemetry_otlp::MetricExporter> {
    use opentelemetry_otlp::MetricExporter;
    use synapse_config::telemetry::exporters::ExportProtocol;

    let exporter = match config.protocol {
        ExportProtocol::Grpc => MetricExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.as_str())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build gRPC metrics exporter: {e}"))?,
        ExportProtocol::HttpProto => MetricExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.as_str())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP metrics exporter: {e}"))?,
    };

    Ok(exporter)
}

/// Initialize OTLP trace export
fn init_tracer(
    config: &TelemetryConfig,
    resource: opentelemetry_sdk::Resource,
) -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};

    let exporter_config = config
        .tracing
        .as_ref()
        .and_then(|t| t.exporter.as_ref())
        .or(config.exporter.as_ref())
        .ok_or_else(|| anyhow::anyhow!("no trace exporter configured"))?;

    let exporter = build_span_exporter(exporter_config)?;

    let sampling_rate = config.tracing.as_ref().map_or(1.0, |t| t.sampling_rate);

    let sampler = if sampling_rate >= 1.0 {
        Sampler::AlwaysOn
    } else if sampling_rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(sampling_rate)
    };

    let use_parent_based = config.tracing.as_ref().is_none_or(|t| t.parent_based);

    let effective_sampler = if use_parent_based {
        Sampler::ParentBased(Box::new(sampler))
    } else {
        sampler
    };

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(effective_sampler)
        .with_batch_exporter(exporter)
        .build();

    Ok(provider)
}

/// Build OTLP span exporter based on protocol
fn build_span_exporter(
    config: &synapse_config::telemetry::exporters::ExporterConfig,
) -> anyhow::Result<opentelemetry_otlp::SpanExporter> {
    use opentelemetry_otlp::SpanExporter;
    use synapse_config::telemetry::exporters::ExportProtocol;

    let exporter = match config.protocol {
        ExportProtocol::Grpc => SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.as_str())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build gRPC span exporter: {e}"))?,
        ExportProtocol::HttpProto => SpanExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.as_str())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP span exporter: {e}"))?,
    };

    Ok(exporter)
}
