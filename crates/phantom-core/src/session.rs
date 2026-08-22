use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
#[cfg(unix)]
use rusqlite::OpenFlags;
use rusqlite::{Connection, MAIN_DB};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::config::MAX_SERIALIZED_APP_CONFIG_BYTES;
use crate::error::{AppError, AppResult};
use crate::MAX_TABS;

pub const MAX_TAB_ID_LEN: usize = 128;
pub const MAX_TAB_TITLE_LEN: usize = 256;
pub const MAX_TAB_CWD_LEN: usize = 4096;
pub const MAX_TAB_PROFILE_ID_LEN: usize = 128;
const REMEMBERED_TABS_LOCK_FILE: &str = "remembered-tabs.lock";
const MAX_APP_CONFIG_BYTES: usize = MAX_SERIALIZED_APP_CONFIG_BYTES;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRecord {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    pub cwd: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub is_active: bool,
    /// Which shell profile this tab was launched with, for faithful restore.
    #[serde(default)]
    pub shell_profile_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl TabRecord {
    pub fn validate(&self) -> AppResult<()> {
        validate_optional_len("tab id", self.id.as_deref(), MAX_TAB_ID_LEN)?;
        validate_len("tab title", &self.title, MAX_TAB_TITLE_LEN)?;
        validate_len("tab cwd", &self.cwd, MAX_TAB_CWD_LEN)?;
        validate_optional_len(
            "tab shell profile id",
            self.shell_profile_id.as_deref(),
            MAX_TAB_PROFILE_ID_LEN,
        )?;
        validate_no_nul("tab title", &self.title)?;
        validate_no_nul("tab cwd", &self.cwd)?;
        if let Some(id) = &self.id {
            validate_no_nul("tab id", id)?;
        }
        if let Some(profile_id) = &self.shell_profile_id {
            validate_no_nul("tab shell profile id", profile_id)?;
        }
        Ok(())
    }
}

/// Process-lifetime ownership of the remembered tab set.
///
/// The operating system releases the advisory lock when this handle is
/// dropped or the process exits, including after a crash. The lock file uses
/// the session store's descriptor-relative path and permission checks.
#[derive(Debug)]
pub struct RememberedTabsLock {
    #[cfg(any(unix, windows))]
    _file: std::fs::File,
}

impl RememberedTabsLock {
    pub fn acquire() -> AppResult<Self> {
        Self::acquire_at(&db_path()?)
    }

    #[cfg(unix)]
    fn acquire_at(database_path: &std::path::Path) -> AppResult<Self> {
        use std::os::fd::AsRawFd;

        let (directory, _) = walk_store_parent(database_path, true)?;
        let lock_path = database_path.with_file_name(REMEMBERED_TABS_LOCK_FILE);
        let file = open_or_create_regular_file(
            &directory,
            std::ffi::OsStr::new(REMEMBERED_TABS_LOCK_FILE),
            &lock_path,
        )?;
        ensure_regular_file(&file, &lock_path, "remembered-tabs lock")?;
        ensure_exact_owner_mode(&file, &lock_path, 0o600, "remembered-tabs lock")?;

        // LOCK_NB keeps a second launch bounded. This descriptor is opened
        // CLOEXEC, so neither spawned shells nor child tools inherit ownership.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(AppError::Other(
                    "another normal Phantom instance already owns remembered tab state".to_string(),
                ));
            }
            return Err(AppError::Other(format!(
                "could not lock remembered tab state at {}: {error}",
                lock_path.display()
            )));
        }

        Ok(Self { _file: file })
    }

    #[cfg(windows)]
    fn acquire_at(database_path: &std::path::Path) -> AppResult<Self> {
        use std::fs::{OpenOptions, TryLockError};
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        prepare_secure_store(database_path)?;
        let lock_path = database_path.with_file_name(REMEMBERED_TABS_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Cooperative instances may open the file, but omitting delete
            // sharing prevents replacement from splitting lock ownership.
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&lock_path)
            .map_err(|error| {
                AppError::Other(format!(
                    "could not open remembered tab lock at {}: {error}",
                    lock_path.display()
                ))
            })?;
        let metadata = file.metadata().map_err(|error| {
            AppError::Other(format!(
                "could not inspect remembered tab lock at {}: {error}",
                lock_path.display()
            ))
        })?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::Other(format!(
                "refusing to use remembered-tabs lock {}: not a regular non-reparse file",
                lock_path.display()
            )));
        }

        // Rust opens File handles as non-inheritable. The OS releases this
        // whole-file lock when the handle closes, including after a crash.
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(AppError::Other(
                "another normal Phantom instance already owns remembered tab state".to_string(),
            )),
            Err(TryLockError::Error(error)) => Err(AppError::Other(format!(
                "could not lock remembered tab state at {}: {error}",
                lock_path.display()
            ))),
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn acquire_at(_database_path: &std::path::Path) -> AppResult<Self> {
        Err(AppError::Other(
            "remembered tab ownership is unsupported on this platform".to_string(),
        ))
    }
}

#[derive(Clone)]
pub struct SessionStore {
    conn: Arc<Mutex<Connection>>,
    // #82 establishes one normal session/config writer. Shared clones belong
    // to that owner: once it observes an unbounded or non-text config row, no
    // clone may write config for the rest of this process lifetime. An
    // external database replacement deliberately cannot re-enable writes.
    config_write_blocked: Arc<AtomicBool>,
}

impl SessionStore {
    pub fn open() -> AppResult<Self> {
        Self::open_at(&db_path()?)
    }

    /// Open (creating if needed) the store at `path`, refusing to proceed
    /// unless owner-only at-rest protection is actually in place: a `0700`
    /// directory holding a `0600` regular-file database, both owned by the
    /// current user, with no symlinks anywhere in the final components. On
    /// failure the caller gets an error and no store — the app then runs
    /// without persistence rather than writing session data somewhere an
    /// attacker could read or redirect.
    fn open_at(path: &std::path::Path) -> AppResult<Self> {
        let secure_store = prepare_secure_store(path)?;
        let conn = open_secure_connection(path, &secure_store)?;
        // Two running instances share this WAL database; without a busy
        // timeout a concurrent write returns SQLITE_BUSY immediately and the
        // save fails with a user-visible notice.
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        migrate(&conn)?;
        // SQLite creates the WAL/SHM sidecars next to the database with the
        // database's own 0600 mode, but re-verify so a loose sidecar left by
        // an earlier build cannot linger inside the store directory.
        verify_sidecars(path, &secure_store)?;
        verify_store_identity(path, &secure_store)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config_write_blocked: Arc::new(AtomicBool::new(false)),
        })
    }

    #[doc(hidden)]
    pub fn in_memory_for_tests() -> AppResult<Self> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config_write_blocked: Arc::new(AtomicBool::new(false)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("session store mutex poisoned")
    }

    pub fn load_tabs(&self) -> AppResult<Vec<TabRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tab_uid, title, cwd, sort_order, is_active, shell_profile_id, created_at, updated_at \
             FROM tabs ORDER BY sort_order",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TabRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                cwd: row.get(2)?,
                sort_order: row.get(3)?,
                is_active: row.get::<_, i64>(4)? != 0,
                shell_profile_id: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        let tabs = rows.collect::<Result<Vec<_>, _>>()?;
        validate_tab_records(&tabs)?;
        Ok(tabs)
    }

    pub fn save_tabs(&self, tabs: &[TabRecord]) -> AppResult<()> {
        if tabs.is_empty() {
            return Ok(());
        }
        validate_tab_records(tabs)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let previous_cwds = previous_cwds_by_id(&tx)?;
        let tabs = stable_tab_records(tabs, &previous_cwds);
        tx.execute("DELETE FROM tabs", [])?;
        for (i, t) in tabs.iter().enumerate() {
            tx.execute(
                "INSERT INTO tabs (tab_uid, title, cwd, sort_order, is_active, shell_profile_id, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    t.id,
                    t.title,
                    t.cwd,
                    i as i64,
                    t.is_active as i64,
                    t.shell_profile_id,
                    t.created_at,
                    t.updated_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_tabs(&self) -> AppResult<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM tabs", [])?;
        Ok(())
    }

    pub fn load_config(&self) -> AppResult<AppConfig> {
        self.load_config_with_limit(MAX_APP_CONFIG_BYTES)
    }

    fn load_config_with_limit(&self, max_bytes: usize) -> AppResult<AppConfig> {
        let conn = self.lock();
        let allow_recovery_writes = !self.config_write_blocked.load(Ordering::Acquire);
        let loaded = load_config_conn(&conn, max_bytes, allow_recovery_writes)?;
        if loaded.write_blocked {
            self.config_write_blocked.store(true, Ordering::Release);
        }
        Ok(loaded.config)
    }

    pub fn save_config(&self, config: &AppConfig) -> AppResult<()> {
        self.save_config_with_locked_hook(config, || {})
    }

    fn save_config_with_locked_hook(
        &self,
        config: &AppConfig,
        after_lock: impl FnOnce(),
    ) -> AppResult<()> {
        let conn = self.lock();
        after_lock();
        if self.config_write_blocked.load(Ordering::Acquire) {
            return Err(AppError::InvalidConfig(
                "refusing to overwrite an oversized or non-text persisted app config".to_string(),
            ));
        }
        config.validate()?;
        save_config_conn(&conn, config)
    }
}

struct LoadedConfig {
    config: AppConfig,
    write_blocked: bool,
}

fn load_config_conn(
    conn: &Connection,
    max_bytes: usize,
    allow_recovery_writes: bool,
) -> AppResult<LoadedConfig> {
    let value = load_config_value(conn, max_bytes)?;
    match value {
        Some(value) if value.storage_type == "text" && !value.exceeds_limit => {
            match serde_json::from_slice::<AppConfig>(&value.bytes)
                .map_err(AppError::from)
                .and_then(AppConfig::validated)
            {
                Ok(config) => Ok(LoadedConfig {
                    config,
                    write_blocked: false,
                }),
                Err(error) => {
                    if allow_recovery_writes {
                        if value.bytes.len() <= MAX_INVALID_CONFIG_BACKUP_VALUE_BYTES {
                            if let Ok(json) = std::str::from_utf8(&value.bytes) {
                                let _ = backup_invalid_config(conn, json, &error.to_string());
                            } else {
                                let _ = backup_omitted_invalid_config(
                                    conn,
                                    value.bytes.len(),
                                    "stored app config is not valid UTF-8",
                                );
                            }
                        } else {
                            let _ = backup_omitted_invalid_config(
                                conn,
                                value.bytes.len(),
                                "stored invalid app config is too large to back up",
                            );
                        }
                    }
                    let default = AppConfig::default().validated()?;
                    if allow_recovery_writes {
                        save_config_conn(conn, &default)?;
                    }
                    Ok(LoadedConfig {
                        config: default,
                        write_blocked: false,
                    })
                }
            }
        }
        Some(_) => {
            // Do not mutate an unbounded row: even changing its key can make
            // SQLite reconstruct the whole record. The store returns safe
            // in-memory defaults and blocks later config writes so automatic
            // persistence cannot destroy the original value.
            Ok(LoadedConfig {
                config: AppConfig::default().validated()?,
                write_blocked: true,
            })
        }
        None => Ok(LoadedConfig {
            config: AppConfig::default().validated()?,
            write_blocked: false,
        }),
    }
}

fn migrate(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA secure_delete=ON;
         CREATE TABLE IF NOT EXISTS tabs (
            id               INTEGER PRIMARY KEY,
            tab_uid          TEXT,
            title            TEXT NOT NULL,
            cwd              TEXT NOT NULL,
            sort_order       INTEGER NOT NULL,
            is_active        INTEGER NOT NULL DEFAULT 0,
            shell_profile_id TEXT,
            created_at       TEXT,
            updated_at       TEXT
         );
         CREATE TABLE IF NOT EXISTS config (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );",
    )?;
    // Additive column migrations for databases created by earlier builds.
    // `CREATE TABLE IF NOT EXISTS` above leaves a pre-existing `tabs` table
    // untouched, so new columns must be added explicitly here.
    add_column_if_missing(conn, "tabs", "shell_profile_id", "TEXT")?;
    add_column_if_missing(conn, "tabs", "tab_uid", "TEXT")?;
    add_column_if_missing(conn, "tabs", "created_at", "TEXT")?;
    add_column_if_missing(conn, "tabs", "updated_at", "TEXT")?;
    Ok(())
}

/// Add `column` (with the given SQL type) to `table` if it does not already
/// exist. Idempotent — safe to run on every launch.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> AppResult<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    drop(stmt);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}"),
            [],
        )?;
    }
    Ok(())
}

fn previous_cwds_by_id(conn: &Connection) -> AppResult<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT tab_uid, cwd FROM tabs WHERE tab_uid IS NOT NULL")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut cwds = HashMap::new();
    for row in rows {
        let (id, cwd) = row?;
        if !id.trim().is_empty() && !cwd.trim().is_empty() {
            cwds.insert(id, cwd);
        }
    }
    Ok(cwds)
}

fn stable_tab_records(
    tabs: &[TabRecord],
    previous_cwds: &HashMap<String, String>,
) -> Vec<TabRecord> {
    let mut seen_ids = HashSet::new();
    let mut active_assigned = false;
    let mut stable = Vec::with_capacity(tabs.len());

    for tab in tabs {
        let id = tab.id.as_ref().and_then(|id| {
            let clean = id.trim();
            if clean.is_empty() || !seen_ids.insert(clean.to_string()) {
                None
            } else {
                Some(clean.to_string())
            }
        });
        let cwd = if tab.cwd.trim().is_empty() {
            id.as_ref()
                .and_then(|id| previous_cwds.get(id))
                .cloned()
                .unwrap_or_default()
        } else {
            tab.cwd.clone()
        };
        let is_active = tab.is_active && !active_assigned;
        active_assigned |= is_active;

        stable.push(TabRecord {
            id,
            title: tab.title.clone(),
            cwd,
            sort_order: 0,
            is_active,
            shell_profile_id: tab.shell_profile_id.clone(),
            created_at: tab.created_at.clone(),
            updated_at: tab.updated_at.clone(),
        });
    }

    if !active_assigned && !stable.is_empty() {
        stable[0].is_active = true;
    }

    stable
}

fn validate_tab_records(tabs: &[TabRecord]) -> AppResult<()> {
    if tabs.len() > MAX_TABS {
        return Err(AppError::Other(format!(
            "no more than {MAX_TABS} tabs can be remembered"
        )));
    }
    for tab in tabs {
        tab.validate()?;
    }
    Ok(())
}

fn validate_len(name: &str, value: &str, max: usize) -> AppResult<()> {
    if value.len() > max {
        return Err(AppError::Other(format!(
            "{name} must be at most {max} bytes"
        )));
    }
    Ok(())
}

fn validate_optional_len(name: &str, value: Option<&str>, max: usize) -> AppResult<()> {
    if let Some(value) = value {
        validate_len(name, value, max)?;
    }
    Ok(())
}

fn validate_no_nul(name: &str, value: &str) -> AppResult<()> {
    if value.contains('\0') {
        return Err(AppError::Other(format!("{name} cannot contain NUL bytes")));
    }
    Ok(())
}

fn save_config_conn(conn: &Connection, config: &AppConfig) -> AppResult<()> {
    let json = serde_json::to_string(config)?;
    if json.len() > MAX_APP_CONFIG_BYTES {
        return Err(AppError::InvalidConfig(format!(
            "serialized app config may be no more than {MAX_APP_CONFIG_BYTES} bytes"
        )));
    }
    conn.execute(
        "INSERT INTO config (key, value) VALUES ('app', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![json],
    )?;
    Ok(())
}

/// Cap on retained `app.invalid.*` backup rows: repeated corrupt-config loads
/// must not grow the table forever.
const MAX_INVALID_CONFIG_BACKUPS: usize = 5;
const MAX_INVALID_CONFIG_BACKUP_VALUE_BYTES: usize = 1024 * 1024;

struct StoredConfigValue {
    storage_type: String,
    bytes: Vec<u8>,
    exceeds_limit: bool,
}

fn load_config_value(conn: &Connection, max_bytes: usize) -> AppResult<Option<StoredConfigValue>> {
    let transaction = conn.unchecked_transaction()?;
    let identity = match transaction.query_row(
        "SELECT rowid, typeof(value) FROM config WHERE key = 'app'",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok(identity) => identity,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            transaction.commit()?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let (row_id, storage_type) = identity;
    if storage_type != "text" {
        transaction.commit()?;
        return Ok(Some(StoredConfigValue {
            storage_type,
            bytes: Vec::new(),
            exceeds_limit: false,
        }));
    }

    let mut blob = transaction.blob_open(MAIN_DB, "config", "value", row_id, true)?;
    if blob.len() > max_bytes {
        drop(blob);
        transaction.commit()?;
        return Ok(Some(StoredConfigValue {
            storage_type,
            bytes: Vec::new(),
            exceeds_limit: true,
        }));
    }
    let mut bytes = vec![0; blob.len()];
    blob.read_exact(&mut bytes)?;
    drop(blob);
    transaction.commit()?;
    Ok(Some(StoredConfigValue {
        storage_type,
        bytes,
        exceeds_limit: false,
    }))
}

fn backup_invalid_config(conn: &Connection, json: &str, reason: &str) -> AppResult<()> {
    let backup = serde_json::json!({
        "reason": reason,
        "value": json,
    });
    insert_invalid_config_backup(conn, backup)
}

fn backup_omitted_invalid_config(
    conn: &Connection,
    observed_bytes: usize,
    reason: &str,
) -> AppResult<()> {
    // The rejected value is deliberately not fetched or copied into its
    // backup: a tampered database must not turn recovery into another large
    // allocation. `observed_bytes` is capped at the caller's limit + 1.
    let backup = serde_json::json!({
        "reason": reason,
        "observed_bytes": observed_bytes,
        "value_omitted": true,
    });
    insert_invalid_config_backup(conn, backup)
}

fn insert_invalid_config_backup(conn: &Connection, backup: serde_json::Value) -> AppResult<()> {
    prune_invalid_config_backups(conn)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let key = format!("app.invalid.{stamp}");
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, backup.to_string()],
    )?;
    Ok(())
}

fn prune_invalid_config_backups(conn: &Connection) -> AppResult<()> {
    // Keep only the newest backups (by insertion order), leaving one slot for
    // the backup about to be added.
    conn.execute(
        "DELETE FROM config WHERE key LIKE 'app.invalid.%' AND rowid NOT IN (
             SELECT rowid FROM config WHERE key LIKE 'app.invalid.%'
             ORDER BY rowid DESC LIMIT ?1
         )",
        rusqlite::params![(MAX_INVALID_CONFIG_BACKUPS - 1) as i64],
    )?;
    Ok(())
}

fn db_path() -> AppResult<PathBuf> {
    let dirs = ProjectDirs::from("com", "phantom", "terminal")
        .ok_or_else(|| AppError::Other("could not resolve app data directory".to_string()))?;
    Ok(dirs.data_dir().join("phantom.db"))
}

/// Handles retained until SQLite has opened and migrated the same verified
/// store. The Unix SQLite API accepts a path rather than an existing fd, so
/// callers re-check path identities after opening; root and same-euid
/// processes remain outside this boundary because they can replace and
/// restore path components between any two checks.
#[cfg(unix)]
struct SecureStoreGuard {
    directory: std::fs::File,
    database: std::fs::File,
    database_name: std::ffi::OsString,
}

/// Establish the store's at-rest protection before SQLite touches the path.
/// Every directory component is traversed relative to an already verified
/// descriptor with `O_NOFOLLOW`, so intermediate symlinks are never followed.
#[cfg(unix)]
fn prepare_secure_store(path: &std::path::Path) -> AppResult<SecureStoreGuard> {
    let (directory, database_name) = walk_store_parent(path, true)?;
    let database = open_or_create_regular_file(&directory, &database_name, path)?;
    ensure_regular_file(&database, path, "session store database")?;
    ensure_exact_owner_mode(&database, path, 0o600, "session store database")?;

    let guard = SecureStoreGuard {
        directory,
        database,
        database_name,
    };
    verify_sidecars(path, &guard)?;
    Ok(guard)
}

#[cfg(not(unix))]
fn prepare_secure_store(path: &std::path::Path) -> AppResult<()> {
    // Phantom Terminal ships on macOS and Linux only; on other targets fall
    // back to the platform's default ACLs, as before this hardening.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_secure_connection(
    path: &std::path::Path,
    guard: &SecureStoreGuard,
) -> AppResult<Connection> {
    let conn =
        Connection::open_with_flags(path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    verify_store_identity(path, guard)?;
    Ok(conn)
}

#[cfg(not(unix))]
fn open_secure_connection(path: &std::path::Path, _guard: &()) -> AppResult<Connection> {
    Ok(Connection::open(path)?)
}

/// Verify the WAL/SHM sidecars (when present) relative to the retained private
/// directory, tightening loose modes and refusing symlinks or special files.
#[cfg(unix)]
fn verify_sidecars(path: &std::path::Path, guard: &SecureStoreGuard) -> AppResult<()> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    for suffix in [b"-wal".as_slice(), b"-shm".as_slice()] {
        let mut name = guard.database_name.as_bytes().to_vec();
        name.extend_from_slice(suffix);
        let name = std::ffi::OsString::from_vec(name);
        let sidecar = path.with_file_name(&name);
        let file = match open_file_at(&guard.directory, &name, false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(probe_error(&sidecar, &error)),
        };
        ensure_regular_file(&file, &sidecar, "session store sidecar")?;
        ensure_exact_owner_mode(&file, &sidecar, 0o600, "session store sidecar")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_sidecars(_path: &std::path::Path, _guard: &()) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_store_identity(path: &std::path::Path, guard: &SecureStoreGuard) -> AppResult<()> {
    use std::os::unix::fs::MetadataExt;

    let (directory, database_name) = walk_store_parent(path, false)?;
    let expected_dir = guard
        .directory
        .metadata()
        .map_err(|error| probe_error(path, &error))?;
    let actual_dir = directory
        .metadata()
        .map_err(|error| probe_error(path, &error))?;
    if expected_dir.dev() != actual_dir.dev() || expected_dir.ino() != actual_dir.ino() {
        return Err(AppError::Other(format!(
            "refusing to use session store {}: directory changed while opening",
            path.display()
        )));
    }
    if database_name != guard.database_name {
        return Err(AppError::Other(format!(
            "refusing to use session store {}: database name changed while opening",
            path.display()
        )));
    }

    let database = open_file_at(&directory, &database_name, false)
        .map_err(|error| probe_error(path, &error))?;
    ensure_regular_file(&database, path, "session store database")?;
    ensure_exact_owner_mode(&database, path, 0o600, "session store database")?;
    let expected_db = guard
        .database
        .metadata()
        .map_err(|error| probe_error(path, &error))?;
    let actual_db = database
        .metadata()
        .map_err(|error| probe_error(path, &error))?;
    if expected_db.dev() != actual_db.dev() || expected_db.ino() != actual_db.ino() {
        return Err(AppError::Other(format!(
            "refusing to use session store {}: database changed while opening",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_store_identity(_path: &std::path::Path, _guard: &()) -> AppResult<()> {
    Ok(())
}

/// Walk to the database's parent from `/`, retaining only directory handles
/// and refusing every symlink. Controlling ancestors must be owned by root or
/// the effective user and must not be group/other-writable. The root-owned
/// sticky-directory exception admits conventional `/tmp`: its sticky bit
/// prevents other users from replacing an entry owned by this user, and any
/// pre-planted foreign-owned child is rejected on the next iteration.
#[cfg(unix)]
fn walk_store_parent(
    path: &std::path::Path,
    create_missing: bool,
) -> AppResult<(std::fs::File, std::ffi::OsString)> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(AppError::Other(format!(
            "session store path {} must be absolute",
            path.display()
        )));
    }
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(AppError::Other(format!(
            "session store path {} has no filesystem root",
            path.display()
        )));
    }
    let mut names = Vec::new();
    for component in components {
        match component {
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::Other(format!(
                    "session store path {} contains an unsafe component",
                    path.display()
                )));
            }
        }
    }
    let database_name = names.pop().ok_or_else(|| {
        AppError::Other(format!(
            "session store path {} has no database file name",
            path.display()
        ))
    })?;
    let mut directory = open_root_directory()?;
    let mut traversed = PathBuf::from("/");
    validate_controlling_directory(&directory, &traversed)?;

    let final_index = names.len().checked_sub(1);
    for (index, name) in names.into_iter().enumerate() {
        traversed.push(&name);
        let next = match open_directory_at(&directory, &name) {
            Ok(next) => next,
            Err(error) if create_missing && error.kind() == std::io::ErrorKind::NotFound => {
                match create_directory_at(&directory, &name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(probe_error(&traversed, &error)),
                }
                open_directory_at(&directory, &name)
                    .map_err(|error| probe_error(&traversed, &error))?
            }
            Err(error) => return Err(probe_error(&traversed, &error)),
        };
        if Some(index) == final_index {
            ensure_exact_owner_mode(&next, &traversed, 0o700, "session store directory")?;
        } else {
            validate_controlling_directory(&next, &traversed)?;
        }
        directory = next;
    }

    // A database directly below `/` would make the root directory the store
    // directory, which cannot satisfy the current user's ownership invariant.
    if final_index.is_none() {
        ensure_exact_owner_mode(&directory, &traversed, 0o700, "session store directory")?;
    }
    Ok((directory, database_name))
}

#[cfg(unix)]
fn open_root_directory() -> AppResult<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open("/")
        .map_err(|error| probe_error(std::path::Path::new("/"), &error))
}

#[cfg(unix)]
fn component_cstring(name: &std::ffi::OsStr) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))
}

#[cfg(unix)]
fn open_directory_at(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = component_cstring(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn create_directory_at(parent: &std::fs::File, name: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let name = component_cstring(name)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn open_file_at(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    create_new: bool,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = component_cstring(name)?;
    let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    if create_new {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn probe_error(path: &std::path::Path, error: &std::io::Error) -> AppError {
    if error.raw_os_error() == Some(libc::ELOOP) {
        return AppError::Other(format!(
            "refusing to use session store path {}: it is a symlink",
            path.display()
        ));
    }
    if error.raw_os_error() == Some(libc::ENOTDIR) {
        return AppError::Other(format!(
            "refusing to use session store path {}: a component is a symlink or not a directory",
            path.display()
        ));
    }
    AppError::Other(format!(
        "could not open session store path {}: {error}",
        path.display()
    ))
}

#[cfg(unix)]
fn open_or_create_regular_file(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    path: &std::path::Path,
) -> AppResult<std::fs::File> {
    let opened = match open_file_at(directory, name, false) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match open_file_at(directory, name, true) {
                Ok(file) => Ok(file),
                // Another app instance may win creation; adopt its inode only
                // after the same handle-based verification.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_file_at(directory, name, false)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    opened.map_err(|error| probe_error(path, &error))
}

#[cfg(target_os = "macos")]
mod mac_acl {
    use std::ffi::{c_int, c_void};
    use std::os::fd::{AsRawFd, RawFd};

    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const ACL_EXTENDED_ALLOW: c_int = 1;

    unsafe extern "C" {
        fn acl_free(object: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_get_tag_type(entry: *mut c_void, tag_type: *mut c_int) -> c_int;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: c_int, acl: *mut c_void, acl_type: c_int) -> c_int;
    }

    struct Acl(*mut c_void);

    impl Drop for Acl {
        fn drop(&mut self) {
            let _ = unsafe { acl_free(self.0) };
        }
    }

    fn load(fd: RawFd) -> std::io::Result<Option<Acl>> {
        let acl = unsafe { acl_get_fd_np(fd, ACL_TYPE_EXTENDED) };
        if !acl.is_null() {
            return Ok(Some(Acl(acl)));
        }
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP) | Some(libc::ENOENT)
        ) {
            // A filesystem with no ACL support cannot grant access beyond its
            // Unix mode bits.
            return Ok(None);
        }
        Err(error)
    }

    fn inspect(fd: RawFd) -> std::io::Result<(bool, bool)> {
        let Some(acl) = load(fd)? else {
            return Ok((false, false));
        };
        let mut entry = std::ptr::null_mut();
        let mut entry_id = ACL_FIRST_ENTRY;
        let mut any = false;
        let mut allowing = false;
        loop {
            let result = unsafe { acl_get_entry(acl.0, entry_id, &mut entry) };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            if result < 0 {
                return Err(std::io::Error::last_os_error());
            }
            any = true;
            let mut tag_type = 0;
            if unsafe { acl_get_tag_type(entry, &mut tag_type) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            allowing |= tag_type == ACL_EXTENDED_ALLOW;
            entry_id = ACL_NEXT_ENTRY;
        }
        Ok((any, allowing))
    }

    pub(super) fn reject_allowing(
        file: &std::fs::File,
        path: &std::path::Path,
    ) -> crate::error::AppResult<()> {
        let (_, allowing) = inspect(file.as_raw_fd()).map_err(|error| {
            crate::error::AppError::Other(format!(
                "could not inspect ACL of session store ancestor {}: {error}",
                path.display()
            ))
        })?;
        if allowing {
            return Err(crate::error::AppError::Other(format!(
                "refusing to use session store ancestor {}: an extended ACL grants additional access",
                path.display()
            )));
        }
        Ok(())
    }

    pub(super) fn clear(
        file: &std::fs::File,
        path: &std::path::Path,
        what: &str,
    ) -> crate::error::AppResult<()> {
        let (any, _) = inspect(file.as_raw_fd()).map_err(|error| {
            crate::error::AppError::Other(format!(
                "could not inspect ACL of {what} {}: {error}",
                path.display()
            ))
        })?;
        if !any {
            return Ok(());
        }

        let empty = unsafe { acl_init(0) };
        if empty.is_null() {
            return Err(crate::error::AppError::Other(format!(
                "could not allocate an empty ACL for {what} {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let empty = Acl(empty);
        if unsafe { acl_set_fd_np(file.as_raw_fd(), empty.0, ACL_TYPE_EXTENDED) } != 0 {
            return Err(crate::error::AppError::Other(format!(
                "could not remove ACL from {what} {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let (remaining, _) = inspect(file.as_raw_fd()).map_err(|error| {
            crate::error::AppError::Other(format!(
                "could not verify ACL of {what} {}: {error}",
                path.display()
            ))
        })?;
        if remaining {
            return Err(crate::error::AppError::Other(format!(
                "refusing to use {what} {}: extended ACL entries could not be removed",
                path.display()
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn has_entries(file: &std::fs::File) -> std::io::Result<bool> {
        inspect(file.as_raw_fd()).map(|(any, _)| any)
    }
}

#[cfg(target_os = "macos")]
fn reject_permissive_ancestor_acl(file: &std::fs::File, path: &std::path::Path) -> AppResult<()> {
    mac_acl::reject_allowing(file, path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reject_permissive_ancestor_acl(_file: &std::fs::File, _path: &std::path::Path) -> AppResult<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn clear_extended_acl(file: &std::fs::File, path: &std::path::Path, what: &str) -> AppResult<()> {
    mac_acl::clear(file, path, what)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn clear_extended_acl(
    _file: &std::fs::File,
    _path: &std::path::Path,
    _what: &str,
) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_controlling_directory(file: &std::fs::File, path: &std::path::Path) -> AppResult<()> {
    use std::os::unix::fs::MetadataExt;

    let meta = file.metadata().map_err(|error| probe_error(path, &error))?;
    if !meta.file_type().is_dir() {
        return Err(AppError::Other(format!(
            "refusing to use session store ancestor {}: not a directory",
            path.display()
        )));
    }
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != 0 && meta.uid() != euid {
        return Err(AppError::Other(format!(
            "refusing to use session store ancestor {}: owned by uid {}, not root or effective uid {euid}",
            path.display(),
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o7777;
    let root_sticky = meta.uid() == 0 && mode & 0o1000 != 0;
    if mode & 0o022 != 0 && !root_sticky {
        return Err(AppError::Other(format!(
            "refusing to use session store ancestor {}: unsafe permissions {mode:04o}",
            path.display()
        )));
    }
    reject_permissive_ancestor_acl(file, path)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_regular_file(file: &std::fs::File, path: &std::path::Path, what: &str) -> AppResult<()> {
    let meta = file.metadata().map_err(|error| probe_error(path, &error))?;
    if !meta.file_type().is_file() {
        return Err(AppError::Other(format!(
            "refusing to use {what} {}: not a regular file",
            path.display()
        )));
    }
    Ok(())
}

/// Require an already no-follow-opened handle to be owned by the effective
/// user with the exact requested mode, tightening it through `fchmod` and
/// rejecting any special bits that remain.
#[cfg(unix)]
fn ensure_exact_owner_mode(
    file: &std::fs::File,
    path: &std::path::Path,
    want: u32,
    what: &str,
) -> AppResult<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let meta = file.metadata().map_err(|error| probe_error(path, &error))?;
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(AppError::Other(format!(
            "refusing to use {what} {}: owned by uid {} but this process runs as uid {euid}",
            path.display(),
            meta.uid()
        )));
    }
    let mut mode = meta.mode() & 0o7777;
    if mode != want && unsafe { libc::fchmod(file.as_raw_fd(), want as libc::mode_t) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(AppError::Other(format!(
            "could not restrict permissions of {what} {}: {error}",
            path.display()
        )));
    }
    clear_extended_acl(file, path, what)?;
    mode = file
        .metadata()
        .map_err(|error| probe_error(path, &error))?
        .mode()
        & 0o7777;
    if mode != want {
        return Err(AppError::Other(format!(
            "refusing to use {what} {}: mode {mode:04o} could not be restricted to {want:04o}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> SessionStore {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        SessionStore {
            conn: Arc::new(Mutex::new(conn)),
            config_write_blocked: Arc::new(AtomicBool::new(false)),
        }
    }

    fn tab(title: &str, cwd: &str, active: bool) -> TabRecord {
        TabRecord {
            id: None,
            title: title.into(),
            cwd: cwd.into(),
            sort_order: 0,
            is_active: active,
            shell_profile_id: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn tabs_roundtrip_preserves_order_and_active() {
        let store = in_memory();
        store
            .save_tabs(&[
                tab("one", "/a", false),
                tab("two", "/b", true),
                tab("three", "/c", false),
            ])
            .unwrap();

        let loaded = store.load_tabs().unwrap();
        let titles: Vec<_> = loaded.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["one", "two", "three"]);
        // sort_order is assigned from insertion index.
        assert_eq!(
            loaded.iter().map(|t| t.sort_order).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(loaded[1].is_active);
        assert!(!loaded[0].is_active && !loaded[2].is_active);
    }

    #[test]
    fn save_tabs_replaces_previous_set() {
        let store = in_memory();
        store.save_tabs(&[tab("old", "/x", true)]).unwrap();
        store.save_tabs(&[tab("new", "/y", false)]).unwrap();

        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "new");
    }

    #[test]
    fn save_tabs_accepts_shared_limit_and_rejects_excess_without_data_loss() {
        let store = in_memory();
        let mut tabs: Vec<_> = (0..MAX_TABS)
            .map(|index| tab(&format!("tab-{index}"), "/known", index == 0))
            .collect();

        store.save_tabs(&tabs).unwrap();
        assert_eq!(store.load_tabs().unwrap().len(), MAX_TABS);

        tabs.push(tab("excess", "/known", false));
        let error = store.save_tabs(&tabs).unwrap_err().to_string();

        assert_eq!(
            error,
            format!("no more than {MAX_TABS} tabs can be remembered")
        );
        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded.len(), MAX_TABS);
        assert_eq!(
            loaded.last().unwrap().title,
            format!("tab-{}", MAX_TABS - 1)
        );
    }

    #[test]
    fn save_tabs_preserves_existing_cwd_when_incoming_cwd_is_unknown() {
        let store = in_memory();
        let mut existing = tab("old", "/known", true);
        existing.id = Some("stable".into());
        store.save_tabs(&[existing]).unwrap();

        let mut incoming = tab("new", "", true);
        incoming.id = Some("stable".into());
        store.save_tabs(&[incoming]).unwrap();

        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded[0].cwd, "/known");
        assert_eq!(loaded[0].title, "new");
    }

    #[test]
    fn save_tabs_ignores_empty_save_requests() {
        let store = in_memory();
        store.save_tabs(&[tab("old", "/known", true)]).unwrap();
        store.save_tabs(&[]).unwrap();

        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].cwd, "/known");
    }

    #[test]
    fn clear_tabs_removes_remembered_tabs() {
        let store = in_memory();
        store.save_tabs(&[tab("old", "/known", true)]).unwrap();

        store.clear_tabs().unwrap();

        assert!(store.load_tabs().unwrap().is_empty());
    }

    #[test]
    fn save_tabs_rejects_oversized_title() {
        let store = in_memory();
        let oversized = tab(&"x".repeat(MAX_TAB_TITLE_LEN + 1), "/known", true);

        assert!(store.save_tabs(&[oversized]).is_err());
    }

    #[test]
    fn save_tabs_normalizes_active_tab_state() {
        let store = in_memory();
        store
            .save_tabs(&[
                tab("one", "/a", true),
                tab("two", "/b", true),
                tab("three", "/c", false),
            ])
            .unwrap();

        let loaded = store.load_tabs().unwrap();
        assert!(loaded[0].is_active);
        assert!(!loaded[1].is_active);
        assert!(!loaded[2].is_active);

        store
            .save_tabs(&[tab("one", "/a", false), tab("two", "/b", false)])
            .unwrap();
        let loaded = store.load_tabs().unwrap();
        assert!(loaded[0].is_active);
        assert!(!loaded[1].is_active);
    }

    #[test]
    fn tabs_roundtrip_preserves_shell_profile_id() {
        let store = in_memory();
        let mut t = tab("dev", "/proj", true);
        t.shell_profile_id = Some("fish".into());
        store.save_tabs(&[t]).unwrap();

        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded[0].shell_profile_id.as_deref(), Some("fish"));
    }

    #[test]
    fn migrate_adds_shell_profile_id_to_legacy_tabs_table() {
        // Simulate a database created by a build that predates the
        // shell_profile_id column.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tabs (
                id          INTEGER PRIMARY KEY,
                title       TEXT NOT NULL,
                cwd         TEXT NOT NULL,
                sort_order  INTEGER NOT NULL,
                is_active   INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO tabs (title, cwd, sort_order, is_active)
             VALUES ('shell', '/home', 0, 1);",
        )
        .unwrap();

        // Running the migration must add the missing column without dropping
        // the existing row.
        migrate(&conn).unwrap();

        let store = SessionStore {
            conn: Arc::new(Mutex::new(conn)),
            config_write_blocked: Arc::new(AtomicBool::new(false)),
        };
        let loaded = store.load_tabs().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].cwd, "/home");
        assert_eq!(loaded[0].shell_profile_id, None);

        // And saving with the new column now succeeds.
        let mut t = tab("dev", "/proj", true);
        t.shell_profile_id = Some("fish".into());
        store.save_tabs(&[t]).unwrap();
        assert_eq!(
            store.load_tabs().unwrap()[0].shell_profile_id.as_deref(),
            Some("fish")
        );
    }

    #[test]
    fn config_defaults_when_absent() {
        let store = in_memory();
        let cfg = store.load_config().unwrap();
        assert_eq!(cfg.font_size, AppConfig::default().font_size);
        assert!(cfg.restore_on_launch);
    }

    #[test]
    fn config_roundtrip_and_upsert() {
        let store = in_memory();
        let mut cfg = AppConfig {
            font_size: 18,
            font_family: "JetBrains Mono".into(),
            window_size: crate::WindowSize::new(1280, 720),
            ..AppConfig::default()
        };
        store.save_config(&cfg).unwrap();

        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.font_size, 18);
        assert_eq!(loaded.font_family, "JetBrains Mono");
        assert_eq!(loaded.window_size, crate::WindowSize::new(1280, 720));

        // Saving again must upsert (PRIMARY KEY key='app'), not error or duplicate.
        cfg.font_size = 11;
        store.save_config(&cfg).unwrap();
        assert_eq!(store.load_config().unwrap().font_size, 11);
    }

    #[test]
    fn context_config_and_trust_roundtrip() {
        let store = in_memory();
        let root = std::env::current_dir().unwrap();
        let source = "version: 1\nname: Test\ntabs:\n  - id: shell\n    title: Shell\n    cwd: .\n";
        let trusted = crate::trust_context_manifest(&root, source.to_string()).unwrap();
        let mut cfg = AppConfig::default();
        cfg.context_actions.panel_collapsed = true;
        cfg.context_actions.sidebar_width = 333;
        cfg.context_actions
            .plugin_mut(crate::MANIFEST_PLUGIN_ID)
            .unwrap()
            .section_collapsed = true;
        cfg.context_actions
            .record_directory_visit(&root, 42)
            .unwrap();
        cfg.trusted_projects.push(trusted.clone());
        let spdeploy_trust = crate::TrustedSpdeployProject {
            root: "/project".to_string(),
            sources: vec![crate::TrustedSpdeploySource {
                relative_path: "deploy.yml".to_string(),
                source: "name: Project\noperation:\n  deploy:\n    stage: []\n".to_string(),
            }],
        };
        cfg.trusted_spdeploy_projects.push(spdeploy_trust.clone());

        store.save_config(&cfg).unwrap();
        let loaded = store.load_config().unwrap();

        assert!(loaded.context_actions.panel_collapsed);
        assert_eq!(loaded.context_actions.sidebar_width, 333);
        assert_eq!(loaded.context_actions.directory_history.len(), 1);
        assert_eq!(loaded.context_actions.directory_history[0].last_visited, 42);
        assert_eq!(
            loaded
                .context_actions
                .plugin(crate::RECENT_DIRECTORIES_PLUGIN_ID)
                .unwrap()
                .order,
            crate::RECENT_DIRECTORIES_PLUGIN_ORDER
        );
        assert_eq!(
            loaded
                .context_actions
                .plugin(crate::MANIFEST_PLUGIN_ID)
                .unwrap()
                .order,
            crate::MANIFEST_PLUGIN_ORDER
        );
        assert_eq!(
            loaded
                .context_actions
                .plugin(crate::SPDEPLOY_PLUGIN_ID)
                .unwrap()
                .order,
            crate::SPDEPLOY_PLUGIN_ORDER
        );
        assert!(
            loaded
                .context_actions
                .plugin(crate::MANIFEST_PLUGIN_ID)
                .unwrap()
                .section_collapsed
        );
        assert_eq!(loaded.trusted_projects, [trusted]);
        assert_eq!(loaded.trusted_spdeploy_projects, [spdeploy_trust]);
    }

    #[test]
    fn save_config_rejects_invalid_config() {
        let store = in_memory();
        let cfg = AppConfig {
            shell_profiles: Vec::new(),
            ..AppConfig::default()
        };
        assert!(store.save_config(&cfg).is_err());
    }

    #[test]
    fn load_config_recovers_from_invalid_json_and_keeps_backup() {
        let store = in_memory();
        {
            let conn = store.lock();
            conn.execute(
                "INSERT INTO config (key, value) VALUES ('app', '{bad json')",
                [],
            )
            .unwrap();
        }

        let cfg = store.load_config().unwrap();
        assert_eq!(cfg.font_size, AppConfig::default().font_size);

        let conn = store.lock();
        let backup_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM config WHERE key LIKE 'app.invalid.%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(backup_count, 1);
    }

    #[test]
    fn oversized_database_config_is_bounded_before_materialization() {
        let store = in_memory();
        {
            let conn = store.lock();
            conn.execute(
                "INSERT INTO config (key, value)
                 VALUES ('app', CAST(zeroblob(8388608) AS TEXT))",
                [],
            )
            .unwrap();
        }

        let loaded = store.load_config_with_limit(1024).unwrap();
        assert_eq!(loaded.font_size, AppConfig::default().font_size);

        let later = AppConfig {
            font_size: 18,
            ..AppConfig::default()
        };
        let error = store.save_config(&later).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));

        let conn = store.lock();
        let retained_after_save: i64 = conn
            .query_row(
                "SELECT length(CAST(value AS BLOB)) FROM config WHERE key = 'app'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_after_save, 8 * 1024 * 1024);
        let replacement = "{bad json";
        conn.execute(
            "UPDATE config SET value = ?1 WHERE key = 'app'",
            rusqlite::params![replacement],
        )
        .unwrap();
        drop(conn);

        let clone = store.clone();
        clone.load_config_with_limit(1024).unwrap();
        assert!(clone.save_config(&later).is_err());
        let conn = clone.lock();
        let retained_invalid: String = conn
            .query_row("SELECT value FROM config WHERE key = 'app'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained_invalid, replacement);
    }

    #[test]
    fn save_rechecks_the_shared_write_block_after_locking() {
        let store = in_memory();
        let shared_block = Arc::clone(&store.config_write_blocked);

        let error = store
            .save_config_with_locked_hook(&AppConfig::default(), move || {
                shared_block.store(true, Ordering::Release);
            })
            .unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        let conn = store.lock();
        let app_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM config WHERE key = 'app'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(app_rows, 0);
    }

    #[test]
    fn null_database_config_recovers_through_the_bounded_path() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO config (key, value) VALUES ('app', NULL);",
        )
        .unwrap();
        let store = SessionStore {
            conn: Arc::new(Mutex::new(conn)),
            config_write_blocked: Arc::new(AtomicBool::new(false)),
        };

        let loaded = store.load_config_with_limit(1024).unwrap();

        assert_eq!(loaded.font_size, AppConfig::default().font_size);
        assert!(store.save_config(&AppConfig::default()).is_err());
        let conn = store.lock();
        let stored_type: String = conn
            .query_row(
                "SELECT typeof(value) FROM config WHERE key = 'app'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_type, "null");
    }

    /// Unique per-test scratch directory (no tempfile dependency), removed on
    /// drop.
    #[cfg(any(unix, windows))]
    struct ScratchDir(PathBuf);

    #[cfg(any(unix, windows))]
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("phantom-session-test-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // macOS exposes /var as a symlink to /private/var; tests exercise
            // store-local symlinks, not that system-level compatibility alias.
            Self(dir.canonicalize().unwrap())
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    #[cfg(any(unix, windows))]
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    #[cfg(unix)]
    fn chmod(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn add_everyone_allow_acl(path: &std::path::Path) {
        let status = std::process::Command::new("chmod")
            .arg("+a")
            .arg("everyone allow read,write,execute,delete")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[cfg(unix)]
    fn open_at_creates_private_dir_and_db() {
        let scratch = ScratchDir::new("create");
        let db = scratch.path().join("data").join("phantom.db");

        let store = SessionStore::open_at(&db).unwrap();
        store.save_tabs(&[tab("one", "/a", true)]).unwrap();
        assert_eq!(store.load_tabs().unwrap().len(), 1);

        assert_eq!(mode_of(db.parent().unwrap()), 0o700);
        assert_eq!(mode_of(&db), 0o600);
        for suffix in ["-wal", "-shm"] {
            let sidecar = scratch
                .path()
                .join("data")
                .join(format!("phantom.db{suffix}"));
            if sidecar.exists() {
                assert_eq!(mode_of(&sidecar), 0o600, "{}", sidecar.display());
            }
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn remembered_tabs_lock_excludes_secondary_and_releases_without_state_changes() {
        let scratch = ScratchDir::new("remembered-lock");
        let db = scratch.path().join("data").join("phantom.db");
        #[cfg(unix)]
        let lock_path = db.with_file_name(REMEMBERED_TABS_LOCK_FILE);

        let owner = RememberedTabsLock::acquire_at(&db).unwrap();
        let primary_store = SessionStore::open_at(&db).unwrap();
        primary_store
            .save_tabs(&[tab("remembered", "/safe", true)])
            .unwrap();

        assert!(run_remembered_lock_child(&db, true).success());

        // An ephemeral secondary may open the store for configuration, but it
        // performs no remembered-tab mutation.
        let ephemeral_store = SessionStore::open_at(&db).unwrap();
        assert_eq!(ephemeral_store.load_tabs().unwrap().len(), 1);
        drop(ephemeral_store);

        let remembered = primary_store.load_tabs().unwrap();
        assert_eq!(remembered.len(), 1);
        assert_eq!(remembered[0].title, "remembered");
        assert_eq!(remembered[0].cwd, "/safe");
        #[cfg(unix)]
        {
            assert_eq!(mode_of(db.parent().unwrap()), 0o700);
            assert_eq!(mode_of(&lock_path), 0o600);
        }

        drop(owner);
        assert!(run_remembered_lock_child(&db, false).success());
        let reacquired = RememberedTabsLock::acquire_at(&db).unwrap();
        assert_eq!(primary_store.load_tabs().unwrap().len(), 1);
        drop(reacquired);
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn remembered_tabs_lock_child() {
        let Some(db) = std::env::var_os("PHANTOM_TEST_REMEMBERED_LOCK_DB").map(PathBuf::from)
        else {
            return;
        };
        let expect_locked = std::env::var_os("PHANTOM_TEST_EXPECT_LOCKED").is_some();
        let result = RememberedTabsLock::acquire_at(&db);
        if expect_locked {
            let error = result.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("another normal Phantom instance"),
                "{error}"
            );
        } else {
            let _owner = result.unwrap();
            if std::env::var_os("PHANTOM_TEST_HOLD_LOCK").is_some() {
                use std::io::{Read, Write};

                println!("PHANTOM_LOCK_READY");
                std::io::stdout().flush().unwrap();
                let _ = std::io::stdin().read(&mut [0_u8]);
            }
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn remembered_tabs_lock_is_released_after_owner_process_is_killed() {
        use std::io::BufRead;
        use std::process::Stdio;

        let scratch = ScratchDir::new("remembered-lock-killed-owner");
        let db = scratch.path().join("data").join("phantom.db");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("session::tests::remembered_tabs_lock_child")
            .arg("--nocapture")
            .env("PHANTOM_TEST_REMEMBERED_LOCK_DB", &db)
            .env("PHANTOM_TEST_HOLD_LOCK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(output.read_line(&mut line).unwrap(), 0);
            if line.contains("PHANTOM_LOCK_READY") {
                break;
            }
        }

        child.kill().unwrap();
        child.wait().unwrap();
        let _reacquired = RememberedTabsLock::acquire_at(&db).unwrap();
    }

    #[cfg(any(unix, windows))]
    fn run_remembered_lock_child(
        db: &std::path::Path,
        expect_locked: bool,
    ) -> std::process::ExitStatus {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("session::tests::remembered_tabs_lock_child")
            .env("PHANTOM_TEST_REMEMBERED_LOCK_DB", db);
        if expect_locked {
            command.env("PHANTOM_TEST_EXPECT_LOCKED", "1");
        } else {
            command.env_remove("PHANTOM_TEST_EXPECT_LOCKED");
        }
        command.status().unwrap()
    }

    #[test]
    #[cfg(unix)]
    fn remembered_tabs_lock_rejects_symlink() {
        let scratch = ScratchDir::new("remembered-lock-symlink");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let target = scratch.path().join("elsewhere.lock");
        std::fs::File::create(&target).unwrap();
        std::os::unix::fs::symlink(&target, data.join(REMEMBERED_TABS_LOCK_FILE)).unwrap();

        let error = RememberedTabsLock::acquire_at(&data.join("phantom.db")).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn open_at_tightens_loose_existing_modes() {
        let scratch = ScratchDir::new("tighten");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o755);
        let db = data.join("phantom.db");
        std::fs::File::create(&db).unwrap();
        chmod(&db, 0o644);
        let wal = data.join("phantom.db-wal");
        std::fs::File::create(&wal).unwrap();
        chmod(&wal, 0o664);

        // Keep the store alive: SQLite removes the WAL sidecar when the last
        // connection closes, and this test asserts on its tightened mode.
        let _store = SessionStore::open_at(&db).unwrap();

        assert_eq!(mode_of(&data), 0o700);
        assert_eq!(mode_of(&db), 0o600);
        assert_eq!(mode_of(&wal), 0o600);
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_symlinked_db() {
        let scratch = ScratchDir::new("symlink-db");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let target = scratch.path().join("elsewhere.db");
        std::fs::File::create(&target).unwrap();
        std::os::unix::fs::symlink(&target, data.join("phantom.db")).unwrap();

        let error = SessionStore::open_at(&data.join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_symlinked_store_dir() {
        let scratch = ScratchDir::new("symlink-dir");
        let real = scratch.path().join("real");
        std::fs::create_dir(&real).unwrap();
        chmod(&real, 0o700);
        let data = scratch.path().join("data");
        std::os::unix::fs::symlink(&real, &data).unwrap();

        let error = SessionStore::open_at(&data.join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_symlinked_intermediate_directory() {
        let scratch = ScratchDir::new("symlink-intermediate");
        let real = scratch.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let error = SessionStore::open_at(&link.join("data").join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("symlink"), "{error}");
        assert!(!real.join("data").exists());
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_writable_controlling_ancestor() {
        let scratch = ScratchDir::new("unsafe-ancestor");
        let unsafe_dir = scratch.path().join("unsafe");
        std::fs::create_dir(&unsafe_dir).unwrap();
        chmod(&unsafe_dir, 0o777);

        let error = SessionStore::open_at(&unsafe_dir.join("data").join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("unsafe permissions"), "{error}");
        assert!(!unsafe_dir.join("data").exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn open_at_rejects_permissive_acl_on_controlling_ancestor() {
        let scratch = ScratchDir::new("unsafe-ancestor-acl");
        let ancestor = scratch.path().join("ancestor");
        std::fs::create_dir(&ancestor).unwrap();
        add_everyone_allow_acl(&ancestor);

        let error = SessionStore::open_at(&ancestor.join("data").join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("extended ACL"), "{error}");
        assert!(!ancestor.join("data").exists());
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_non_regular_db_path() {
        let scratch = ScratchDir::new("non-regular");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        // A directory where the database file should be.
        std::fs::create_dir(data.join("phantom.db")).unwrap();

        assert!(SessionStore::open_at(&data.join("phantom.db")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_symlinked_wal_sidecar() {
        let scratch = ScratchDir::new("symlink-wal");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let target = scratch.path().join("stolen-wal");
        std::fs::File::create(&target).unwrap();
        std::os::unix::fs::symlink(&target, data.join("phantom.db-wal")).unwrap();

        let error = SessionStore::open_at(&data.join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_symlinked_shm_sidecar() {
        let scratch = ScratchDir::new("symlink-shm");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let target = scratch.path().join("stolen-shm");
        std::fs::File::create(&target).unwrap();
        std::os::unix::fs::symlink(&target, data.join("phantom.db-shm")).unwrap();

        let error = SessionStore::open_at(&data.join("phantom.db"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn open_at_rejects_non_regular_sidecar() {
        let scratch = ScratchDir::new("non-regular-sidecar");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        std::fs::create_dir(data.join("phantom.db-wal")).unwrap();

        assert!(SessionStore::open_at(&data.join("phantom.db")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn open_at_clears_special_mode_bits() {
        let scratch = ScratchDir::new("special-bits");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o1700);
        let db = data.join("phantom.db");
        std::fs::File::create(&db).unwrap();
        chmod(&db, 0o4600);

        let _store = SessionStore::open_at(&db).unwrap();

        assert_eq!(mode_of(&data), 0o700);
        assert_eq!(mode_of(&db), 0o600);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn open_at_removes_extended_acls_from_store_objects() {
        let scratch = ScratchDir::new("strip-acl");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let db = data.join("phantom.db");
        std::fs::File::create(&db).unwrap();
        chmod(&db, 0o600);
        add_everyone_allow_acl(&data);
        add_everyone_allow_acl(&db);

        let _store = SessionStore::open_at(&db).unwrap();

        let directory = std::fs::File::open(&data).unwrap();
        let database = std::fs::File::open(&db).unwrap();
        assert!(!mac_acl::has_entries(&directory).unwrap());
        assert!(!mac_acl::has_entries(&database).unwrap());
        assert_eq!(mode_of(&data), 0o700);
        assert_eq!(mode_of(&db), 0o600);
    }

    #[test]
    #[cfg(unix)]
    fn identity_check_rejects_a_different_database_inode() {
        use std::ffi::OsStr;

        let scratch = ScratchDir::new("identity");
        let data = scratch.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chmod(&data, 0o700);
        let db = data.join("phantom.db");
        std::fs::File::create(&db).unwrap();
        chmod(&db, 0o600);
        let other = data.join("other.db");
        std::fs::File::create(&other).unwrap();
        chmod(&other, 0o600);

        let (directory, database_name) = walk_store_parent(&db, false).unwrap();
        let other_file = open_file_at(&directory, OsStr::new("other.db"), false).unwrap();
        let guard = SecureStoreGuard {
            directory,
            database: other_file,
            database_name,
        };

        let error = verify_store_identity(&db, &guard).unwrap_err();
        assert!(error.to_string().contains("database changed"), "{error}");
    }

    #[test]
    fn invalid_config_backups_are_pruned_to_the_newest_few() {
        let store = in_memory();
        let conn = store.lock();
        conn.execute("INSERT INTO config (key, value) VALUES ('app', 'live')", [])
            .unwrap();
        for i in 0..MAX_INVALID_CONFIG_BACKUPS + 3 {
            conn.execute(
                "INSERT INTO config (key, value) VALUES (?1, 'x')",
                rusqlite::params![format!("app.invalid.seed{i}")],
            )
            .unwrap();
        }

        backup_invalid_config(&conn, "{bad json", "parse error").unwrap();

        let keys: Vec<String> = conn
            .prepare("SELECT key FROM config WHERE key LIKE 'app.invalid.%' ORDER BY rowid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(keys.len(), MAX_INVALID_CONFIG_BACKUPS);
        // The oldest seeds are gone; the newest survive alongside the fresh
        // backup, and the live 'app' row is untouched.
        assert!(!keys.iter().any(|key| key == "app.invalid.seed0"));
        assert_eq!(keys[0], "app.invalid.seed4");
        assert_eq!(keys[keys.len() - 2], "app.invalid.seed7");
        let live: String = conn
            .query_row("SELECT value FROM config WHERE key = 'app'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(live, "live");
    }
}
