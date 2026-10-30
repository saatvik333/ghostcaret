//! API keys in the OS keychain: service "ghostcaret", account = provider id.
//!
//! Keys are read at the moment a provider is built and never logged, stored
//! in settings or sent back to the web UI.

use keyring::{Entry, Error};

const SERVICE: &str = "ghostcaret";

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("the system keychain is unavailable: {0}")]
    Unavailable(String),
    #[error("the system keychain refused the request: {0}")]
    Failed(String),
}

fn entry(provider_id: &str) -> Result<Entry, KeychainError> {
    Entry::new(SERVICE, provider_id).map_err(|e| KeychainError::Unavailable(describe(&e)))
}

/// Error text without the secret: some keyring errors carry the stored bytes,
/// and platform errors are reduced to their kind.
fn describe(e: &Error) -> String {
    match e {
        Error::NoStorageAccess(_) => "access denied or the keychain is locked".into(),
        Error::PlatformFailure(_) => "platform error".into(),
        Error::NoDefaultStore => "no keychain service is running".into(),
        Error::BadEncoding(_) | Error::BadDataFormat(..) => "the stored key is unreadable".into(),
        Error::Ambiguous(_) => "more than one stored key matches".into(),
        Error::NoEntry => "no key stored".into(),
        Error::TooLong(attr, limit) => format!("`{attr}` is longer than {limit}"),
        Error::BadStoreFormat(_) | Error::Invalid(..) | Error::NotSupportedByStore(_) => e.to_string(),
        _ => "keychain error".into(),
    }
}

pub fn get(provider_id: &str) -> Result<Option<String>, KeychainError> {
    match entry(provider_id)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(KeychainError::Failed(describe(&e))),
    }
}

pub fn set(provider_id: &str, key: &str) -> Result<(), KeychainError> {
    entry(provider_id)?.set_password(key).map_err(|e| KeychainError::Failed(describe(&e)))
}

pub fn delete(provider_id: &str) -> Result<(), KeychainError> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError::Failed(describe(&e))),
    }
}

pub fn has(provider_id: &str) -> bool {
    matches!(get(provider_id), Ok(Some(_)))
}
