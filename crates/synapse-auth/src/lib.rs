mod error;
mod resolver;
pub mod usage;
pub mod vault;
pub mod vault_events;

pub use error::AuthError;
pub use resolver::{ApiKeyResolver, KeyMode, ProviderKeyRef, RateLimits, ResolvedKey};
pub use usage::{UsageEvent, UsageReporter};
pub use vault::{VaultClient, VaultError, VaultKey};
pub use vault_events::{VaultEvent, handle_vault_event};
