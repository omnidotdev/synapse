use std::{sync::OnceLock, time::Duration};

use axum::http;
use reqwest::Client;

/// Common HTTP client to reuse connections across TTS providers
pub fn http_client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();

    CLIENT
        .get_or_init(|| {
            let mut headers = http::HeaderMap::new();
            headers.insert(http::header::CONNECTION, http::HeaderValue::from_static("keep-alive"));

            Client::builder()
                .timeout(Duration::from_secs(120))
                .pool_idle_timeout(Some(Duration::from_secs(5)))
                .tcp_nodelay(true)
                .tcp_keepalive(Some(Duration::from_secs(60)))
                .default_headers(headers)
                .build()
                .expect("Failed to build default HTTP client")
        })
        .clone()
}
