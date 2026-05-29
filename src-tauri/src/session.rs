use std::path::PathBuf;
use std::sync::Mutex;

use directories::ProjectDirs;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRecord {
    pub title: String,
    pub cwd: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub is_active: bool,
    /// Which shell profile this tab was launched with, for faithful restore.
    #[serde(default)]
    pub shell_profile_id: Option<String>,
}

pub struct SessionStore {
    conn: Mutex<Connection>,
}

impl SessionStore {
    pub fn open() -> AppResult<Self> {
        let path = db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        restrict_permissions(&path);
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("session store mutex poisoned")
    }

    pub fn load_tabs(&self) -> AppResult<Vec<TabRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT title, cwd, sort_order, is_active, shell_profile_id \
             FROM tabs ORDER BY sort_order",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TabRecord {
                title: row.get(0)?,
                cwd: row.get(1)?,
                sort_order: row.get(2)?,
                is_active: row.get::<_, i64>(3)? != 0,
                shell_profile_id: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn save_tabs(&self, tabs: &[TabRecord]) -> AppResult<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM tabs", [])?;
        for (i, t) in tabs.iter().enumerate() {
            tx.execute(
                "INSERT INTO tabs (title, cwd, sort_order, is_active, shell_profile_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    t.title,
                    t.cwd,
                    i as i64,
                    t.is_active as i64,
                    t.shell_profile_id
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_config(&self) -> AppResult<AppConfig> {
        let conn = self.lock();
        let value: Option<String> = conn
            .query_row("SELECT value FROM config WHERE key = 'app'", [], |r| {
                r.get(0)
            })
            .ok();
        match value {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(AppConfig::default()),
        }
    }

    pub fn save_config(&self, config: &AppConfig) -> AppResult<()> {
        let json = serde_json::to_string(config)?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO config (key, value) VALUES ('app', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![json],
        )?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS tabs (
            id               INTEGER PRIMARY KEY,
            title            TEXT NOT NULL,
            cwd              TEXT NOT NULL,
            sort_order       INTEGER NOT NULL,
            is_active        INTEGER NOT NULL DEFAULT 0,
            shell_profile_id TEXT
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

fn db_path() -> AppResult<PathBuf> {
    let dirs = ProjectDirs::from("com", "phantom", "terminal")
        .ok_or_else(|| AppError::Other("could not resolve app data directory".to_string()))?;
    Ok(dirs.data_dir().join("phantom.db"))
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> SessionStore {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        SessionStore {
            conn: Mutex::new(conn),
        }
    }

    fn tab(title: &str, cwd: &str, active: bool) -> TabRecord {
        TabRecord {
            title: title.into(),
            cwd: cwd.into(),
            sort_order: 0,
            is_active: active,
            shell_profile_id: None,
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
            conn: Mutex::new(conn),
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
            ..AppConfig::default()
        };
        store.save_config(&cfg).unwrap();

        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.font_size, 18);
        assert_eq!(loaded.font_family, "JetBrains Mono");

        // Saving again must upsert (PRIMARY KEY key='app'), not error or duplicate.
        cfg.font_size = 11;
        store.save_config(&cfg).unwrap();
        assert_eq!(store.load_config().unwrap().font_size, 11);
    }
}
