//! Wire format types for provider-specific API protocols
//!
//! Each module contains pure serde structs matching the respective provider's
//! JSON API format. These types are only used for serialization/deserialization
//! at the boundary and are not used internally.

pub mod anthropic;
pub mod google;
pub mod openai;
