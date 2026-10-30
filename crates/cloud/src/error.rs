use std::fmt;
use std::time::Duration;

use oft_engine::complete::CompleteError;

/// Why a provider call failed. `Display` is the text to show the user: for
/// rejected requests it is the provider's own error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// The provider configuration cannot be used (bad URL, missing key).
    Config(String),
    Cancelled,
    /// The provider answered 429, or the client-side per-minute limit is used up.
    RateLimited {
        message: String,
        retry_after: Option<Duration>,
    },
    /// Transient: connection failure, timeout, 5xx, overload.
    Unavailable(String),
    /// The provider refused the request (bad key, unknown model, invalid
    /// parameters). `status` is `None` for errors reported inside a stream.
    Rejected {
        status: Option<u16>,
        message: String,
    },
    /// The provider answered, but not in the documented shape.
    Malformed(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Config(m) => f.write_str(m),
            ProviderError::Cancelled => f.write_str("cancelled"),
            ProviderError::RateLimited { message, .. } => write!(f, "rate limited: {message}"),
            ProviderError::Unavailable(m) => write!(f, "unavailable: {m}"),
            ProviderError::Rejected {
                status: Some(s),
                message,
            } => write!(f, "{message} (HTTP {s})"),
            ProviderError::Rejected {
                status: None,
                message,
            } => f.write_str(message),
            ProviderError::Malformed(m) => write!(f, "unexpected response: {m}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl ProviderError {
    /// Worth another attempt for a request whose answer is still wanted.
    pub(crate) fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. } | ProviderError::Unavailable(_)
        )
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderError::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl From<ProviderError> for CompleteError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::Cancelled => CompleteError::Cancelled,
            ProviderError::RateLimited { .. } => CompleteError::RateLimited,
            ProviderError::Unavailable(m) => CompleteError::Unavailable(m),
            other => CompleteError::Failed(other.to_string()),
        }
    }
}
