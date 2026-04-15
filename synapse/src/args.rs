use std::path::PathBuf;

use clap::Parser;

/// Synapse AI Router
#[derive(Debug, Parser)]
#[command(name = "synapse", about = "Unified AI router for LLM, MCP, STT, and TTS")]
pub struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "synapse.toml", env = "SYNAPSE_CONFIG")]
    pub config: PathBuf,

    /// Override the listen address
    #[arg(long, env = "SYNAPSE_LISTEN")]
    pub listen: Option<std::net::SocketAddr>,
}
