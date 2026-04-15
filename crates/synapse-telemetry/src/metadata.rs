use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::resource as semconv;
use synapse_config::TelemetryConfig;

/// Build an OpenTelemetry Resource from configuration
pub fn build_resource(config: &TelemetryConfig) -> Resource {
    let mut attrs = vec![
        KeyValue::new(semconv::SERVICE_NAME, config.service_name.clone()),
        KeyValue::new(semconv::SERVICE_VERSION, env!("CARGO_PKG_VERSION").to_string()),
    ];

    for (key, value) in &config.resource_attributes {
        attrs.push(KeyValue::new(key.clone(), value.clone()));
    }

    Resource::builder().with_attributes(attrs).build()
}
