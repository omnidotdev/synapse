#![allow(dead_code)]
//! Test server wrapper that starts Synapse on a random port

use std::net::SocketAddr;

use synapse_config::Config;
use synapse_server::Server;
use tokio_util::sync::CancellationToken;

/// A running test server instance
pub struct TestServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    client: reqwest::Client,
}

impl TestServer {
    /// Start a test server with the given configuration
    ///
    /// Binds to port 0 for automatic port assignment
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        let server = Server::new(config).await?;
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        // Bind the listener here so we know the actual port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        tokio::spawn(async move {
            axum::serve(listener, server.into_router())
                .with_graceful_shutdown(async move {
                    shutdown_clone.cancelled().await;
                })
                .await
                .ok();
        });

        let client = reqwest::Client::new();

        Ok(Self { addr, shutdown, client })
    }

    /// Base URL of the running test server
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Get a reference to the HTTP client
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Server address
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
