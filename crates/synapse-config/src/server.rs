use std::net::SocketAddr;

use serde::Deserialize;

use crate::{
    client_identification::ClientIdentificationConfig, client_ip::ClientIpConfig, cors::CorsConfig, csrf::CsrfConfig,
    health::HealthConfig, oauth::OAuthConfig, rate_limit::RateLimitConfig, tls::TlsConfig,
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen_address: Option<SocketAddr>,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub cors: Option<CorsConfig>,
    #[serde(default)]
    pub csrf: Option<CsrfConfig>,
    #[serde(default)]
    pub oauth: Option<OAuthConfig>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub client_identification: Option<ClientIdentificationConfig>,
    #[serde(default)]
    pub client_ip: Option<ClientIpConfig>,
}
