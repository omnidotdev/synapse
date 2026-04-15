//! Bidirectional conversion between internal canonical types and wire formats
//!
//! Each submodule handles conversions for a specific provider's protocol.

pub mod anthropic;
pub mod google;
pub mod openai;
