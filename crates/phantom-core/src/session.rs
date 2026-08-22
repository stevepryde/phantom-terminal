use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};

pub const MAX_TAB_RECORDS: usize = 128;
pub const MAX_TAB_ID_LEN: usize = 128;
pub const MAX_TAB_TITLE_LEN: usize = 256;
pub const MAX_TAB_CWD_LEN: usize = 4096;
pub const MAX_TAB_PROFILE_ID_LEN: usize = 128;

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

#[derive(Clone)]
pub struct SessionStore {
    conn: Arc<Mutex<Connection>>,
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
        prepare_secure_store(path)?;
        let conn = Connection::open(path)?;
        // Two running instances share this WAL database; without a busy
        // timeout a concurrent write returns SQLITE_BUSY immediately and the
        // save fails with a user-visible notice.
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        migrate(&conn)?;
        // SQLite creates the WAL/SHM sidecars next to the database with the
        // database's own 0600 mode, but re-verify so a loose sidecar left by
        // an earlier build cannot linger inside the store directory.
        verify_sidecars(path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[doc(hidden)]
    pub fn in_memory_for_tests() -> AppResult<Self> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
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
        let conn = self.lock();
        let value: Option<String> =
            match conn.query_row("SELECT value FROM config WHERE key = 'app'", [], |r| {
                r.get(0)
            }) {
                Ok(value) => Some(value),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(error) => return Err(error.into()),
            };
        match value {
            Some(json) => match serde_json::from_str::<AppConfig>(&json)
                .map_err(AppError::from)
                .and_then(AppConfig::validated)
            {
                Ok(config) => Ok(config),
                Err(error) => {
                    let _ = backup_invalid_config(&conn, &json, &error.to_string());
                    let default = AppConfig::default().validated()?;
                    save_config_conn(&conn, &default)?;
                    Ok(default)
                }
            },
            None => Ok(AppConfig::default().validated()?),
        }
    }

    pub fn save_config(&self, config: &AppConfig) -> AppResult<()> {
        let conn = self.lock();
        config.validate()?;
        save_config_conn(&conn, config)
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
    if tabs.len() > MAX_TAB_RECORDS {
        return Err(AppError::Other(format!(
            "no more than {MAX_TAB_RECORDS} tabs can be remembered"
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

fn backup_invalid_config(conn: &Connection, json: &str, reason: &str) -> AppResult<()> {
    // Keep only the newest backups (by insertion order), counting the row
    // about to be inserted.
    conn.execute(
        "DELETE FROM config WHERE key LIKE 'app.invalid.%' AND rowid NOT IN (
             SELECT rowid FROM config WHERE key LIKE 'app.invalid.%'
             ORDER BY rowid DESC LIMIT ?1
         )",
        rusqlite::params![(MAX_INVALID_CONFIG_BACKUPS - 1) as i64],
    )?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let key = format!("app.invalid.{stamp}");
    let backup = serde_json::json!({
        "reason": reason,
        "value": json,
    });
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, backup.to_string()],
    )?;
    Ok(())
}

fn db_path() -> AppResult<PathBuf> {
    let dirs = ProjectDirs::from("com", "phantom", "terminal")
        .ok_or_else(|| AppError::Other("could not resolve app data directory".to_string()))?;
    Ok(dirs.data_dir().join("phantom.db"))
}

/// Establish the store's at-rest protection before SQLite touches the path,
/// failing closed on any doubt. The directory and database are created with
/// their final modes (no create-then-chmod window), existing paths are opened
/// with `O_NOFOLLOW` and verified through the opened handle's metadata
/// (rejecting symlinks, non-regular files, and foreign ownership), and modes
/// looser than owner-only are tightened via `fchmod` and re-checked. The
/// remaining path/handle races are only reachable by processes that can
/// already write inside the verified `0700` directory, i.e. the same user.
#[cfg(unix)]
fn prepare_secure_store(path: &std::path::Path) -> AppResult<()> {
    use std::os::unix::fs::DirBuilderExt;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            AppError::Other(format!(
                "session store path {} has no parent directory",
                path.display()
            ))
        })?;
    // mkdir(2) applies the mode at creation (masked by umask, which can only
    // tighten it); verification below repairs or rejects anything looser.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|error| {
            AppError::Other(format!(
                "could not create session store directory {}: {error}",
                parent.display()
            ))
        })?;
    let dir =
        open_probe(parent, ProbeKind::Directory).map_err(|error| probe_error(parent, &error))?;
    ensure_owner_only(&dir, parent, 0o700, "session store directory")?;

    let db = open_or_create_db_file(path)?;
    if !db
        .metadata()
        .map_err(|error| probe_error(path, &error))?
        .file_type()
        .is_file()
    {
        return Err(AppError::Other(format!(
            "refusing to use session store database {}: not a regular file",
            path.display()
        )));
    }
    ensure_owner_only(&db, path, 0o600, "session store database")?;

    verify_sidecars(path)
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

/// Verify the WAL/SHM sidecars (when present) are owner-only regular files,
/// tightening loose modes and refusing symlinks or special files.
#[cfg(unix)]
fn verify_sidecars(path: &std::path::Path) -> AppResult<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(AppError::Other(format!(
            "session store path {} has no file name",
            path.display()
        )));
    };
    for suffix in ["-wal", "-shm"] {
        let sidecar = path.with_file_name(format!("{name}{suffix}"));
        let file = match open_probe(&sidecar, ProbeKind::File) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(probe_error(&sidecar, &error)),
        };
        if !file
            .metadata()
            .map_err(|error| probe_error(&sidecar, &error))?
            .file_type()
            .is_file()
        {
            return Err(AppError::Other(format!(
                "refusing to use session store sidecar {}: not a regular file",
                sidecar.display()
            )));
        }
        ensure_owner_only(&file, &sidecar, 0o600, "session store sidecar")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_sidecars(_path: &std::path::Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
enum ProbeKind {
    Directory,
    File,
    CreateFile,
}

/// Open `path` without following a final-component symlink, so every check
/// afterwards can go through the opened handle (`fstat`/`fchmod`) instead of
/// a re-raceable path. `O_NONBLOCK` keeps a planted FIFO from hanging the
/// probe open; it has no effect once the target is proven a regular file.
#[cfg(unix)]
fn open_probe(path: &std::path::Path, kind: ProbeKind) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    match kind {
        ProbeKind::Directory => {
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
        }
        ProbeKind::File => {
            options
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        ProbeKind::CreateFile => {
            options
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
    }
    options.open(path)
}

#[cfg(unix)]
fn probe_error(path: &std::path::Path, error: &std::io::Error) -> AppError {
    if error.raw_os_error() == Some(libc::ELOOP) {
        return AppError::Other(format!(
            "refusing to use session store path {}: it is a symlink",
            path.display()
        ));
    }
    AppError::Other(format!(
        "could not open session store path {}: {error}",
        path.display()
    ))
}

/// Open the database file, creating it atomically with mode `0600` when it
/// does not exist yet (`O_CREAT|O_EXCL`, so a concurrently planted path can
/// never be silently adopted).
#[cfg(unix)]
fn open_or_create_db_file(path: &std::path::Path) -> AppResult<std::fs::File> {
    let opened = match open_probe(path, ProbeKind::File) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match open_probe(path, ProbeKind::CreateFile) {
                Ok(file) => Ok(file),
                // Lost a create race with another instance of this app;
                // the now-existing file still gets fully verified.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_probe(path, ProbeKind::File)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    opened.map_err(|error| probe_error(path, &error))
}

/// Require `file` (already opened `O_NOFOLLOW`) to be owned by the current
/// user with exactly `want` permission bits, tightening a looser mode through
/// the handle and failing if the result still is not owner-only.
#[cfg(unix)]
fn ensure_owner_only(
    file: &std::fs::File,
    path: &std::path::Path,
    want: u32,
    what: &str,
) -> AppResult<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;

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
    if mode != want {
        if unsafe { libc::fchmod(file.as_raw_fd(), want as libc::mode_t) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(AppError::Other(format!(
                "could not restrict permissions of {what} {}: {error}",
                path.display()
            )));
        }
        mode = file
            .metadata()
            .map_err(|error| probe_error(path, &error))?
            .mode()
            & 0o7777;
    }
    if mode & 0o077 != 0 || mode & want != want {
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

    /// Unique per-test scratch directory (no tempfile dependency), removed on
    /// drop.
    #[cfg(unix)]
    struct ScratchDir(PathBuf);

    #[cfg(unix)]
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("phantom-session-test-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    #[cfg(unix)]
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
