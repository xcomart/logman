//! Saved SSH session profiles and their JSON-backed store.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths::{config_file, strip_bom, write_atomic};

/// Default SSH port, omitted from [`SessionProfile::label`].
const DEFAULT_SSH_PORT: u16 = 22;

/// How logman authenticates against a host.
///
/// Serialized as an internally tagged enum, e.g.
/// `{"kind":"public_key","key_path":"/home/me/.ssh/id_ed25519"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    /// Keyboard-interactive / password authentication.
    Password,
    /// Public key authentication using the private key at `key_path`.
    PublicKey {
        /// Path of the private key file to offer to the server.
        key_path: PathBuf,
    },
    /// Delegate authentication to a running SSH agent.
    Agent,
}

/// Address a tunnel listener binds to when the profile does not say.
///
/// Loopback rather than `0.0.0.0`, for the same reason OpenSSH defaults `-L`
/// that way: a forwarded port carries whatever the remote service trusts its
/// own network to hold, and exposing it to the local network hands that trust
/// to every machine on the segment.
fn default_bind_address() -> String {
    "127.0.0.1".to_owned()
}

/// One local port forwarding rule, the equivalent of OpenSSH's `-L`.
///
/// A connection to `bind_address:local_port` is carried over the session's own
/// transport and opened, by the remote host, to `remote_host:remote_port`. The
/// remote address is therefore resolved *there*: a name that only exists inside
/// the remote network is exactly the point of the rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelRule {
    /// Address the local listener binds; loopback unless the user edited it.
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    /// Local TCP port to listen on.
    pub local_port: u16,
    /// Host to connect to from the remote end, as the remote end resolves it.
    pub remote_host: String,
    /// TCP port to connect to on `remote_host`.
    pub remote_port: u16,
}

/// Per-session deviations from the global [`crate::AppSettings`].
///
/// Every field is optional: `None` means "inherit whatever the global settings
/// say". Overrides are resolved by
/// [`AppSettings::effective_terminal`](crate::AppSettings::effective_terminal),
/// which also re-applies the global clamps, so a hand-edited profile cannot
/// smuggle an absurd font size past the settings validation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionOverrides {
    /// Color scheme id for this session only.
    pub scheme: Option<String>,
    /// Font size for this session only.
    pub font_size: Option<f32>,
    /// Scrollback depth for this session only.
    pub scrollback_lines: Option<usize>,
    /// `TERM` value advertised to this host only.
    pub term: Option<String>,
}

impl SessionOverrides {
    /// Whether nothing is overridden, so the session runs on global settings.
    ///
    /// Used as the `skip_serializing_if` predicate for
    /// [`SessionProfile::overrides`], which keeps `profiles.json` free of empty
    /// `"overrides": {}` noise.
    pub fn is_empty(&self) -> bool {
        self.scheme.is_none()
            && self.font_size.is_none()
            && self.scrollback_lines.is_none()
            && self.term.is_none()
    }
}

/// A single saved connection: where to connect and how to authenticate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionProfile {
    /// Stable identifier; also the account key used by [`crate::SecretStore`].
    pub id: Uuid,
    /// Human-readable name shown in the UI.
    pub name: String,
    /// Hostname or IP address of the SSH server.
    pub host: String,
    /// TCP port of the SSH server.
    pub port: u16,
    /// Login user on the remote host.
    pub username: String,
    /// Authentication method to use for this profile.
    pub auth: AuthMethod,
    /// Whether the password (or key passphrase) is kept in the OS keychain.
    pub save_secret: bool,
    /// Per-session overrides; `None` fields inherit the global settings.
    ///
    /// Absent from older `profiles.json` files and omitted again when nothing
    /// is overridden.
    #[serde(default, skip_serializing_if = "SessionOverrides::is_empty")]
    pub overrides: SessionOverrides,
    /// Local port forwardings opened once this session's shell is up.
    ///
    /// Absent from older `profiles.json` files and omitted again while the list
    /// is empty, which is what the great majority of profiles look like.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tunnels: Vec<TunnelRule>,
}

impl SessionProfile {
    /// Create a profile with a freshly generated identifier.
    ///
    /// `save_secret` starts out disabled, no settings are overridden and no
    /// port is forwarded; enable the first explicitly before storing a secret
    /// with [`crate::SecretStore::set`].
    pub fn new(
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        auth: AuthMethod,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            host: host.into(),
            port,
            username: username.into(),
            auth,
            save_secret: false,
            overrides: SessionOverrides::default(),
            tunnels: Vec::new(),
        }
    }

    /// Connection target in `user@host` form, with `:port` appended when the
    /// port is not the SSH default (22).
    pub fn label(&self) -> String {
        if self.port == DEFAULT_SSH_PORT {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, self.port)
        }
    }
}

/// Collection of saved [`SessionProfile`]s, persisted as JSON.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProfileStore {
    /// Profiles in user-visible order.
    #[serde(default)]
    profiles: Vec<SessionProfile>,
}

impl ProfileStore {
    /// Load the store from the default configuration file.
    ///
    /// A missing file is not an error: it yields an empty store, which is what a
    /// first run looks like.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined, the file
    /// cannot be read, or its contents are not valid JSON.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_file()?)
    }

    /// Load the store from an explicit path.
    ///
    /// A missing file yields an empty store.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not contain valid JSON.
    pub fn load_from(path: &Path) -> Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_slice(strip_bom(&data))
            .with_context(|| format!("failed to parse profiles from {}", path.display()))
    }

    /// Write the store to the default configuration file.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined or created,
    /// or when the file cannot be written.
    pub fn save(&self) -> Result<()> {
        self.save_to(&config_file()?)
    }

    /// Write the store to an explicit path, creating parent directories.
    ///
    /// The write is atomic: the data lands in a temporary sibling file that is
    /// then renamed over `path`.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created or the file cannot be
    /// written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize profiles")?;
        write_atomic(path, &json)
    }

    /// All profiles, in insertion order.
    pub fn profiles(&self) -> &[SessionProfile] {
        &self.profiles
    }

    /// Look up a profile by identifier.
    pub fn get(&self, id: Uuid) -> Option<&SessionProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Insert `profile`, replacing an existing entry with the same identifier.
    ///
    /// Replacement keeps the original position in the list.
    pub fn upsert(&mut self, profile: SessionProfile) {
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    /// Remove the profile with the given identifier and return it.
    pub fn remove(&mut self, id: Uuid) -> Option<SessionProfile> {
        let index = self.profiles.iter().position(|p| p.id == id)?;
        Some(self.profiles.remove(index))
    }

    /// Number of stored profiles.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Whether the store holds no profiles.
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> SessionProfile {
        SessionProfile::new(name, "example.com", 22, "alice", AuthMethod::Password)
    }

    #[test]
    fn label_omits_default_port() {
        let profile = sample("prod");
        assert_eq!(profile.label(), "alice@example.com");
    }

    #[test]
    fn label_includes_non_default_port() {
        let profile = SessionProfile::new(
            "staging",
            "example.com",
            2222,
            "bob",
            AuthMethod::PublicKey {
                key_path: PathBuf::from("/home/bob/.ssh/id_ed25519"),
            },
        );
        assert_eq!(profile.label(), "bob@example.com:2222");
    }

    #[test]
    fn new_assigns_unique_ids_and_defaults() {
        let a = sample("a");
        let b = sample("b");
        assert_ne!(a.id, b.id);
        assert!(!a.save_secret);
    }

    #[test]
    fn auth_method_serde_round_trip() {
        let cases = [
            AuthMethod::Password,
            AuthMethod::PublicKey {
                key_path: PathBuf::from("/home/alice/.ssh/id_rsa"),
            },
            AuthMethod::Agent,
        ];
        for auth in cases {
            let json = serde_json::to_string(&auth).expect("serialize");
            let back: AuthMethod = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(auth, back, "round trip failed for {json}");
        }
    }

    #[test]
    fn auth_method_tag_naming() {
        assert_eq!(
            serde_json::to_value(AuthMethod::Password).unwrap(),
            serde_json::json!({ "kind": "password" })
        );
        assert_eq!(
            serde_json::to_value(AuthMethod::Agent).unwrap(),
            serde_json::json!({ "kind": "agent" })
        );
        let value = serde_json::to_value(AuthMethod::PublicKey {
            key_path: PathBuf::from("key"),
        })
        .unwrap();
        assert_eq!(value["kind"], "public_key");
        assert_eq!(value["key_path"], "key");
    }

    #[test]
    fn save_to_load_from_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("profiles.json");

        let mut store = ProfileStore::default();
        let mut first = sample("first");
        first.save_secret = true;
        let second = SessionProfile::new("second", "10.0.0.1", 2200, "root", AuthMethod::Agent);
        store.upsert(first.clone());
        store.upsert(second.clone());

        store.save_to(&path).expect("save");
        let loaded = ProfileStore::load_from(&path).expect("load");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.profiles(), &[first, second]);
    }

    #[test]
    fn save_to_overwrites_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        ProfileStore::default().save_to(&path).expect("first save");

        let mut store = ProfileStore::default();
        store.upsert(sample("only"));
        store.save_to(&path).expect("second save");

        assert_eq!(ProfileStore::load_from(&path).expect("load").len(), 1);
    }

    #[test]
    fn upsert_replaces_same_id_in_place() {
        let mut store = ProfileStore::default();
        let keep = sample("keep");
        let mut original = sample("original");
        store.upsert(keep.clone());
        store.upsert(original.clone());

        original.name = "renamed".to_string();
        original.port = 2022;
        store.upsert(original.clone());

        assert_eq!(store.len(), 2);
        assert_eq!(store.profiles()[0].id, keep.id);
        assert_eq!(
            store.get(original.id).map(|p| p.name.as_str()),
            Some("renamed")
        );
    }

    #[test]
    fn remove_returns_the_profile() {
        let mut store = ProfileStore::default();
        let profile = sample("victim");
        store.upsert(profile.clone());

        assert!(!store.is_empty());
        assert_eq!(store.remove(profile.id), Some(profile.clone()));
        assert!(store.is_empty());
        assert_eq!(store.remove(profile.id), None);
        assert_eq!(store.get(profile.id), None);
    }

    #[test]
    fn load_from_missing_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ProfileStore::load_from(&dir.path().join("nope.json")).expect("load");
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn load_from_tolerates_a_utf8_bom() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let mut store = ProfileStore::default();
        store.upsert(sample("bom"));
        store.save_to(&path).expect("save");

        // Rewrite the file the way a Windows editor would.
        let saved = std::fs::read(&path).expect("read");
        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend_from_slice(&saved);
        std::fs::write(&path, with_bom).expect("write");

        let loaded = ProfileStore::load_from(&path).expect("load");
        assert_eq!(loaded.profiles()[0].name, "bom");
    }

    /// A `profiles.json` written before session overrides existed.
    const LEGACY_PROFILES_JSON: &str = r#"{
      "profiles": [
        {
          "id": "0e6d2a08-3a1f-4a2e-9c0b-6f7f1b2c3d4e",
          "name": "legacy",
          "host": "example.com",
          "port": 22,
          "username": "alice",
          "auth": { "kind": "password" },
          "save_secret": true
        }
      ]
    }"#;

    #[test]
    fn legacy_profiles_without_overrides_still_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, LEGACY_PROFILES_JSON).expect("write");

        let store = ProfileStore::load_from(&path).expect("load legacy profiles");
        assert_eq!(store.len(), 1);
        let profile = &store.profiles()[0];
        assert_eq!(profile.name, "legacy");
        assert!(profile.save_secret);
        assert_eq!(profile.overrides, SessionOverrides::default());
        assert!(profile.overrides.is_empty());
        assert!(profile.tunnels.is_empty());
    }

    #[test]
    fn empty_tunnels_are_not_written_to_disk() {
        let mut store = ProfileStore::default();
        store.upsert(sample("plain"));

        let json = serde_json::to_string(&store).expect("serialize");
        assert!(
            !json.contains("tunnels"),
            "an empty tunnel list must be skipped, got {json}"
        );
    }

    #[test]
    fn tunnel_rules_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let mut profile = sample("forwarder");
        profile.tunnels = vec![
            TunnelRule {
                bind_address: default_bind_address(),
                local_port: 15432,
                remote_host: "db.internal".to_string(),
                remote_port: 5432,
            },
            TunnelRule {
                bind_address: "0.0.0.0".to_string(),
                local_port: 8080,
                remote_host: "127.0.0.1".to_string(),
                remote_port: 80,
            },
        ];

        let mut store = ProfileStore::default();
        store.upsert(profile.clone());
        store.save_to(&path).expect("save");

        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("db.internal"), "got {saved}");

        let loaded = ProfileStore::load_from(&path).expect("load");
        assert_eq!(loaded.profiles(), &[profile]);
    }

    #[test]
    fn tunnel_rule_without_bind_address_defaults_to_loopback() {
        // Hand-edited files are expected to leave the address out, since it is
        // the one field a rule almost never needs to set.
        let rule: TunnelRule = serde_json::from_str(
            r#"{ "local_port": 15432, "remote_host": "db.internal", "remote_port": 5432 }"#,
        )
        .expect("parse");
        assert_eq!(rule.bind_address, "127.0.0.1");
    }

    #[test]
    fn empty_overrides_are_not_written_to_disk() {
        let mut store = ProfileStore::default();
        store.upsert(sample("plain"));

        let json = serde_json::to_string(&store).expect("serialize");
        assert!(
            !json.contains("overrides"),
            "empty overrides must be skipped, got {json}"
        );
    }

    #[test]
    fn non_empty_overrides_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let mut profile = sample("custom");
        profile.overrides = SessionOverrides {
            font_size: Some(18.0),
            term: Some("xterm".to_string()),
            ..SessionOverrides::default()
        };
        assert!(!profile.overrides.is_empty());

        let mut store = ProfileStore::default();
        store.upsert(profile.clone());
        store.save_to(&path).expect("save");

        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("overrides"), "got {saved}");

        let loaded = ProfileStore::load_from(&path).expect("load");
        assert_eq!(loaded.profiles(), &[profile]);
    }

    #[test]
    fn unknown_profile_fields_are_ignored() {
        // A file written by a future version must not break an older build.
        let json = LEGACY_PROFILES_JSON.replace(
            "\"save_secret\": true",
            "\"save_secret\": true, \"future_field\": { \"nested\": 1 }",
        );
        let store: ProfileStore = serde_json::from_str(&json).expect("parse");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn load_from_invalid_json_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, b"not json").expect("write");
        assert!(ProfileStore::load_from(&path).is_err());
    }
}
