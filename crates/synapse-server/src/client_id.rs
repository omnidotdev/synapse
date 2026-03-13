use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use synapse_config::{ClientIdSource, ClientIdentificationConfig, GroupIdSource};
use synapse_core::{Authentication, ClientIdentity};

/// Middleware that extracts client identity from the request
///
/// Uses the configured sources (JWT claims or HTTP headers) to identify
/// the client and determine their group membership.
pub async fn client_id_middleware(config: ClientIdentificationConfig, request: Request, next: Next) -> Response {
    let identity = extract_identity(&config, &request);

    // Store identity in request extensions for downstream handlers
    let mut request = request;
    if let Some(identity) = identity {
        request.extensions_mut().insert(identity);
    }

    next.run(request).await
}

fn extract_identity(config: &ClientIdentificationConfig, request: &Request) -> Option<ClientIdentity> {
    let client_id = match &config.client_id {
        ClientIdSource::Header { name } => request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string),
        ClientIdSource::JwtClaim { path } => {
            let auth = request.extensions().get::<Authentication>()?;
            let token = auth.synapse.as_ref()?;
            token.claims().custom.get_claim(path)
        }
    }?;

    let group = config.group_id.as_ref().and_then(|g| extract_group(g, request));

    Some(ClientIdentity { client_id, group })
}

fn extract_group(source: &GroupIdSource, request: &Request) -> Option<String> {
    match source {
        GroupIdSource::Header { name, allowed } => {
            let value = request
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(std::string::ToString::to_string)?;

            if allowed.is_empty() || allowed.contains(&value) {
                Some(value)
            } else {
                None
            }
        }
        GroupIdSource::JwtClaim { path, allowed } => {
            let auth = request.extensions().get::<Authentication>()?;
            let token = auth.synapse.as_ref()?;
            let value = token.claims().custom.get_claim(path)?;

            if allowed.is_empty() || allowed.contains(&value) {
                Some(value)
            } else {
                None
            }
        }
    }
}
