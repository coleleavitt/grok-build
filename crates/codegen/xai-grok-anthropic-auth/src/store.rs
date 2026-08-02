//! On-disk credential store: the [`AccountData`] schema, rotation selection,
//! and an [`AccountStore`] that persists atomically with symlink and
//! auth-loss guards (a faithful subset of the reference plugin's storage).

use crate::account::Account;
use crate::error::{AnthropicAuthError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The health score at or above which the currently-active account is kept
/// (sticky) rather than re-sorting the pool.
const STICKY_HEALTH_THRESHOLD: i32 = 3;

/// The opencode-compatible account file Grok/JFC share inside the neutral
/// Anthropic account directory.
const SHARED_STORE_FILE_NAME: &str = "anthropic-accounts.json";

/// The root of the persisted credential file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountData {
    /// Schema version (currently 1).
    pub version: u32,
    /// The stored accounts.
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// Index of the active account (defaults to 0 when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_index: Option<usize>,
}

impl Default for AccountData {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: Vec::new(),
            active_index: None,
        }
    }
}

impl AccountData {
    /// The active account (`active_index`, defaulting to 0), bounds-checked.
    pub fn active(&self) -> Option<&Account> {
        let idx = self.active_index.unwrap_or(0);
        self.accounts.get(idx).or_else(|| self.accounts.first())
    }

    /// Find an account by its (unique) name.
    pub fn find(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    /// Find a mutable account by name.
    pub fn find_mut(&mut self, name: &str) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.name == name)
    }

    /// Point `active_index` at the named account, if present.
    pub fn set_active(&mut self, name: &str) {
        if let Some(idx) = self.accounts.iter().position(|a| a.name == name) {
            self.active_index = Some(idx);
        }
    }

    /// Select the account to use at `now`: prefer the request-ready pool, else
    /// the usable pool. Keep the active account when it is a candidate and
    /// healthy (sticky); otherwise pick the best by health, then tier, then
    /// name.
    pub fn select(&self, now: DateTime<Utc>) -> Option<&Account> {
        let ready: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.is_request_ready(now))
            .collect();
        let pool = if ready.is_empty() {
            self.accounts.iter().filter(|a| a.is_usable(now)).collect()
        } else {
            ready
        };
        if pool.is_empty() {
            return None;
        }
        if let Some(active) = self.active_index.and_then(|_| self.active())
            && active.health_score() >= STICKY_HEALTH_THRESHOLD
            && pool.iter().any(|a| a.name == active.name)
        {
            return Some(active);
        }
        // Prefer higher health, then higher tier, then lexicographically
        // smaller name. `max_by` treats "greater" as preferred, so the name
        // comparison is reversed.
        pool.into_iter().max_by(|a, b| {
            a.health_score()
                .cmp(&b.health_score())
                .then_with(|| a.tier_rank().cmp(&b.tier_rank()))
                .then_with(|| b.name.cmp(&a.name))
        })
    }
}

/// A handle to the credential file on disk.
#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
}

impl AccountStore {
    /// Store at an explicit path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Store at the shared Anthropic account path, adopting a live legacy
    /// Grok/JFC/opencode store first when the shared file is missing or dead.
    pub fn from_env() -> Self {
        Self {
            path: resolve_store_path(),
        }
    }

    /// The resolved store path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the store, returning an empty default when the file is absent.
    /// Refuses to read through a symlink.
    pub fn load(&self) -> Result<AccountData> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(AnthropicAuthError::StoreIsSymlink);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AccountData::default());
            }
            Err(e) => return Err(e.into()),
        }
        let bytes = std::fs::read(&self.path)?;
        let data: AccountData = serde_json::from_slice(&bytes)?;
        Ok(data)
    }

    /// Persist the store atomically (write to a temp file in the same
    /// directory, then rename over the target). Refuses to write through a
    /// symlink or to wipe the last account.
    pub fn save(&self, data: &AccountData) -> Result<()> {
        // Auth-loss guard: never turn a non-empty store into an empty one.
        if data.accounts.is_empty()
            && let Ok(existing) = self.load()
            && !existing.accounts.is_empty()
        {
            return Err(AnthropicAuthError::WouldDeleteAllAccounts);
        }
        // Symlink guard on an existing target.
        if let Ok(meta) = std::fs::symlink_metadata(&self.path)
            && meta.file_type().is_symlink()
        {
            return Err(AnthropicAuthError::StoreIsSymlink);
        }
        if let Some(parent) = self.path.parent() {
            create_dir_private(parent)?;
        }
        let serialized = serde_json::to_vec_pretty(data)?;
        let tmp = self.temp_path();
        write_private(&tmp, &serialized)?;
        // Atomic replace.
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        set_file_private(&self.path);
        Ok(())
    }

    /// Load, apply a mutation, and persist. Returns the persisted data.
    pub fn read_modify_write<F>(&self, modify: F) -> Result<AccountData>
    where
        F: FnOnce(&mut AccountData),
    {
        let mut data = self.load()?;
        modify(&mut data);
        self.save(&data)?;
        Ok(data)
    }

    fn temp_path(&self) -> PathBuf {
        let pid = std::process::id();
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_default();
        name.push(format!(".{pid}.tmp"));
        self.path.with_file_name(name)
    }
}

/// Resolve the store path from the shared SDK env override, then the neutral
/// `$HOME/.anthropic-accounts/anthropic-accounts.json` location.
fn resolve_store_path() -> PathBuf {
    if let Some(explicit) =
        std::env::var_os(anthropic::store::STORE_FILE_ENV).filter(|v| !v.is_empty())
    {
        return PathBuf::from(explicit);
    }
    let home = home_dir();
    migrate_legacy_store(&home);
    shared_store_path(&home)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn shared_store_path(home: &Path) -> PathBuf {
    home.join(anthropic::store::STORE_DIR_NAME)
        .join(SHARED_STORE_FILE_NAME)
}

fn legacy_store_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(grok_home) = std::env::var_os("GROK_HOME").filter(|v| !v.is_empty()) {
        paths.push(PathBuf::from(grok_home).join(SHARED_STORE_FILE_NAME));
    }
    paths.push(home.join(".grok").join(SHARED_STORE_FILE_NAME));
    paths.push(
        home.join(".config")
            .join("jfc")
            .join(SHARED_STORE_FILE_NAME),
    );
    paths.push(
        home.join(".config")
            .join("opencode")
            .join(SHARED_STORE_FILE_NAME),
    );
    paths
}

fn store_usable_account_count(path: &Path) -> usize {
    AccountStore::at(path)
        .load()
        .map(|data| {
            data.accounts
                .iter()
                .filter(|a| a.is_enabled() && !a.refresh_token.expose().is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn migrate_legacy_store(home: &Path) {
    let shared = shared_store_path(home);
    if store_usable_account_count(&shared) > 0 {
        return;
    }

    let Some((usable, source)) = legacy_store_paths(home)
        .into_iter()
        .filter(|path| path != &shared)
        .map(|path| (store_usable_account_count(&path), path))
        .filter(|(usable, _)| *usable > 0)
        .max_by_key(|(usable, _)| *usable)
    else {
        return;
    };

    if shared.exists() {
        let backup = shared.with_extension(format!(
            "json.bak-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default()
        ));
        if std::fs::copy(&shared, backup).is_err() {
            return;
        }
    }

    copy_store_into_shared(&source, &shared, usable);
}

fn copy_store_into_shared(source: &Path, shared: &Path, _usable: usize) {
    let Some(parent) = shared.parent() else {
        return;
    };
    if create_dir_private(parent).is_err() {
        return;
    }
    if std::fs::copy(source, shared).is_ok() {
        set_file_private(shared);
    }
}

#[cfg(unix)]
fn create_dir_private(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.exists() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_dir_private(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(unix)]
fn set_file_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, RoutingStatus};
    use crate::token::{AccessToken, RefreshToken};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    fn ready_account(name: &str) -> Account {
        let mut a = Account::new(
            name,
            RefreshToken::new("sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345"),
        );
        a.access_token = Some(AccessToken::new(
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345",
        ));
        a.expires_at = Some(Utc.timestamp_opt(now().timestamp() + 3600, 0).unwrap());
        a.unified_status = Some(RoutingStatus::Allowed);
        a
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "grok-anthropic-auth-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn select_prefers_request_ready_and_is_sticky_when_healthy() {
        let mut data = AccountData::default();
        data.accounts.push(ready_account("a"));
        data.accounts.push(ready_account("b"));
        data.active_index = Some(1); // "b" active, healthy (Allowed → score 4+2)
        let chosen = data.select(now()).unwrap();
        assert_eq!(chosen.name, "b", "healthy active account is sticky");
    }

    #[test]
    fn select_missing_active_index_does_not_stick_to_first_account() {
        let mut data = AccountData::default();
        data.accounts.push(ready_account("b"));
        data.accounts.push(ready_account("a"));
        let chosen = data.select(now()).unwrap();
        assert_eq!(
            chosen.name, "a",
            "implicit index 0 is a display fallback, not a rotation pin"
        );
    }

    #[test]
    fn select_deprioritizes_recent_rate_limited_account() {
        let mut data = AccountData::default();
        let mut rate_limited = ready_account("a-rate-limited");
        rate_limited.unified_status = Some(RoutingStatus::Rejected);
        rate_limited.rate_limit_reset_time = Some(0);
        rate_limited.last_auth_error = Some("429 rate_limit_error".to_owned());
        let mut clean = ready_account("b-clean");
        clean.unified_status = Some(RoutingStatus::Rejected);
        clean.rate_limit_reset_time = Some(0);
        data.accounts.push(rate_limited);
        data.accounts.push(clean);

        assert_eq!(data.select(now()).unwrap().name, "b-clean");
    }

    #[test]
    fn select_falls_back_to_usable_when_none_ready() {
        let mut data = AccountData::default();
        let mut expired = ready_account("stale");
        expired.expires_at = Some(Utc.timestamp_opt(now().timestamp() - 10, 0).unwrap());
        data.accounts.push(expired);
        // Not request-ready (expired) but usable (can refresh).
        assert_eq!(data.select(now()).unwrap().name, "stale");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("grok-anthropic-auth-test-{}", std::process::id()));
        let path = dir.join("anthropic-accounts.json");
        let store = AccountStore::at(&path);
        let mut data = AccountData::default();
        data.accounts.push(ready_account("primary"));
        store.save(&data).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].name, "primary");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_to_wipe_all_accounts() {
        let dir =
            std::env::temp_dir().join(format!("grok-anthropic-auth-wipe-{}", std::process::id()));
        let path = dir.join("anthropic-accounts.json");
        let store = AccountStore::at(&path);
        let mut data = AccountData::default();
        data.accounts.push(ready_account("primary"));
        store.save(&data).unwrap();

        let empty = AccountData::default();
        let err = store.save(&empty).unwrap_err();
        assert!(matches!(err, AnthropicAuthError::WouldDeleteAllAccounts));
        // The existing account survives.
        assert_eq!(store.load().unwrap().accounts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty_default() {
        let store = AccountStore::at("/nonexistent/dir/does-not-exist.json");
        let data = store.load().unwrap();
        assert!(data.accounts.is_empty());
        assert_eq!(data.version, 1);
    }

    #[test]
    fn shared_store_path_uses_neutral_anthropic_dir() {
        assert_eq!(
            shared_store_path(Path::new("/tmp/home")),
            PathBuf::from("/tmp/home/.anthropic-accounts/anthropic-accounts.json")
        );
    }

    #[test]
    fn migrate_legacy_store_adopts_existing_grok_login() {
        let home = temp_home("migrate-grok");
        let legacy = home.join(".grok").join("anthropic-accounts.json");
        let mut data = AccountData::default();
        data.accounts.push(ready_account("from-grok"));
        AccountStore::at(&legacy).save(&data).unwrap();

        migrate_legacy_store(&home);

        let shared = shared_store_path(&home);
        let loaded = AccountStore::at(&shared).load().unwrap();
        assert_eq!(loaded.accounts[0].name, "from-grok");
        assert!(legacy.exists(), "migration copies, never moves");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn migrate_legacy_store_replaces_dead_shared_with_live_sibling() {
        let home = temp_home("migrate-dead");
        let shared = shared_store_path(&home);
        let mut dead = AccountData::default();
        let mut dead_account = ready_account("dead");
        dead_account.enabled = Some(false);
        dead.accounts.push(dead_account);
        AccountStore::at(&shared).save(&dead).unwrap();

        let sibling = home
            .join(".config")
            .join("jfc")
            .join("anthropic-accounts.json");
        let mut live = AccountData::default();
        live.accounts.push(ready_account("live-a"));
        live.accounts.push(ready_account("live-b"));
        AccountStore::at(&sibling).save(&live).unwrap();

        migrate_legacy_store(&home);

        let loaded = AccountStore::at(&shared).load().unwrap();
        assert_eq!(loaded.accounts.len(), 2);
        assert_eq!(loaded.accounts[0].name, "live-a");
        let backups: Vec<_> = std::fs::read_dir(shared.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }
}
