use http::Method;
use http::header::HeaderName;
use synapse_config::{AnyOrArray, CorsConfig};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// Build a Tower CORS layer from configuration
pub fn cors_layer(config: &CorsConfig) -> CorsLayer {
    let mut layer = CorsLayer::new();

    // Origins
    layer = match &config.origins {
        AnyOrArray::Any => layer.allow_origin(AllowOrigin::any()),
        AnyOrArray::List(origins) => {
            let origins: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
            layer.allow_origin(origins)
        }
    };

    // Methods
    layer = match &config.methods {
        AnyOrArray::Any => layer.allow_methods(AllowMethods::any()),
        AnyOrArray::List(methods) => {
            let methods: Vec<Method> = methods.iter().filter_map(|m| m.parse().ok()).collect();
            layer.allow_methods(methods)
        }
    };

    // Headers
    layer = match &config.headers {
        AnyOrArray::Any => layer.allow_headers(AllowHeaders::any()),
        AnyOrArray::List(headers) => {
            let headers: Vec<HeaderName> = headers.iter().filter_map(|h| h.parse().ok()).collect();
            layer.allow_headers(headers)
        }
    };

    // Expose headers
    if !config.expose_headers.is_empty() {
        let headers: Vec<HeaderName> = config.expose_headers.iter().filter_map(|h| h.parse().ok()).collect();
        layer = layer.expose_headers(headers);
    }

    // Credentials
    if config.credentials {
        layer = layer.allow_credentials(true);
    }

    // Max age
    if let Some(duration) = config.max_age_duration() {
        layer = layer.max_age(duration);
    }

    // Private network access
    if config.private_network {
        layer = layer.allow_private_network(true);
    }

    layer
}
