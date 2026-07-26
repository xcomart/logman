//! Storage of connection secrets in the operating system keychain.
//!
//! Secrets are keyed by the [`SessionProfile`](crate::SessionProfile) identifier
//! inside the `dev.logman.logman` service namespace, so the profile database on
//! disk never contains a password.
//!
//! The backing store is the platform default provided by `keyring` 4.x: the
//! Windows Credential Manager, the macOS Keychain, or the freedesktop Secret
//! Service. Machines without any of those (a headless Linux box, for instance)
//! are supported in a degraded mode: [`init`] reports the failure and
//! [`SecretStore::get`] then behaves as if no secret had ever been saved.

use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use keyring::{Entry, Error as KeyringError};
use uuid::Uuid;

/// Service namespace used for every credential logman stores.
const SERVICE: &str = "dev.logman.logman";

/// Account name used by [`init`] to force the credential store to load.
///
/// Building an entry never creates a credential, so this leaves no trace in the
/// keychain.
const INIT_PROBE_ACCOUNT: &str = "__logman_store_probe__";

/// Cached outcome of the first [`init`] call: `None` on success, otherwise the
/// rendered error.
static INIT_OUTCOME: OnceLock<Option<String>> = OnceLock::new();

/// Install the platform credential store.
///
/// Call this once during start-up. Repeated calls are cheap and return the same
/// result as the first one; the store is installed at most once per process.
///
/// # Errors
///
/// Fails when the platform has no usable credential store (a locked or absent
/// Secret Service, for example). Callers may ignore the error and keep running:
/// [`SecretStore::get`] degrades to "no stored secret" in that case, while
/// [`SecretStore::set`] reports the failure.
pub fn init() -> Result<()> {
    // `keyring::Entry::new` installs the platform default store the first time
    // it runs, which is the only way this crate exposes that step.
    let outcome = INIT_OUTCOME.get_or_init(|| match Entry::new(SERVICE, INIT_PROBE_ACCOUNT) {
        Ok(_) => None,
        Err(err) => Some(err.to_string()),
    });
    match outcome {
        None => Ok(()),
        Some(err) => Err(anyhow!("no usable credential store on this system: {err}")),
    }
}

/// Accessor for the OS keychain, keyed by profile identifier.
///
/// The type is a namespace only; there is nothing to construct.
pub struct SecretStore;

impl SecretStore {
    /// Build the keychain entry for `id`, or `None` when no store is installed.
    fn entry(id: Uuid) -> Result<Option<Entry>> {
        match Entry::new(SERVICE, &id.to_string()) {
            Ok(entry) => Ok(Some(entry)),
            Err(KeyringError::NoDefaultStore) => Ok(None),
            Err(err) => Err(anyhow!("failed to address keychain entry for {id}: {err}")),
        }
    }

    /// Read the secret saved for the profile `id`.
    ///
    /// Returns `Ok(None)` when nothing is stored, and also when the platform has
    /// no usable keychain at all, so that the application keeps working without
    /// one.
    ///
    /// # Errors
    ///
    /// Fails only when a working keychain refuses the read (locked store,
    /// denied access, non-UTF-8 payload).
    pub fn get(id: Uuid) -> Result<Option<String>> {
        if let Err(err) = init() {
            log::warn!("treating secret for {id} as absent: {err:#}");
            return Ok(None);
        }
        let Some(entry) = Self::entry(id)? else {
            return Ok(None);
        };
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) => Err(anyhow!("failed to read keychain entry for {id}: {err}")),
        }
    }

    /// Save `secret` for the profile `id`, replacing any previous value.
    ///
    /// # Errors
    ///
    /// Fails when no credential store is available or when the store rejects
    /// the write. Unlike [`SecretStore::get`] this never fails silently: a
    /// secret the user asked to save must not vanish unnoticed.
    pub fn set(id: Uuid, secret: &str) -> Result<()> {
        init()?;
        let entry = Self::entry(id)?
            .ok_or_else(|| anyhow!("no credential store available to save the secret for {id}"))?;
        entry
            .set_password(secret)
            .map_err(|err| anyhow!("failed to save keychain entry for {id}: {err}"))
    }

    /// Delete the secret saved for the profile `id`.
    ///
    /// Deleting a secret that does not exist succeeds, as does deleting on a
    /// machine without a credential store: in both cases nothing is left behind.
    ///
    /// # Errors
    ///
    /// Fails when a working keychain refuses the deletion.
    pub fn delete(id: Uuid) -> Result<()> {
        if let Err(err) = init() {
            log::warn!("nothing to delete for {id}: {err:#}");
            return Ok(());
        }
        let Some(entry) = Self::entry(id)? else {
            return Ok(());
        };
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(err) => Err(anyhow!("failed to delete keychain entry for {id}: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_namespace_matches_the_project_id() {
        assert_eq!(SERVICE, "dev.logman.logman");
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn init_is_idempotent() {
        // Installing the store may legitimately fail (headless CI), but the
        // answer must be stable across calls and must never panic.
        let first = init().is_ok();
        let second = init().is_ok();
        assert_eq!(first, second);
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn get_of_unknown_id_is_none() {
        // On a machine with no keychain this exercises the degraded path; with
        // one, it exercises the `NoEntry` path. Either way: `Ok(None)`.
        let missing = SecretStore::get(Uuid::new_v4()).expect("get must not fail");
        assert_eq!(missing, None);
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn set_get_delete_round_trip() {
        init().expect("credential store");
        let id = Uuid::new_v4();

        assert_eq!(SecretStore::get(id).expect("get missing"), None);

        SecretStore::set(id, "hunter2").expect("set");
        assert_eq!(SecretStore::get(id).expect("get"), Some("hunter2".into()));

        SecretStore::set(id, "hunter3").expect("overwrite");
        assert_eq!(SecretStore::get(id).expect("get"), Some("hunter3".into()));

        SecretStore::delete(id).expect("delete");
        assert_eq!(SecretStore::get(id).expect("get deleted"), None);
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn delete_missing_entry_is_ok() {
        init().expect("credential store");
        SecretStore::delete(Uuid::new_v4()).expect("delete of missing entry must succeed");
    }
}
