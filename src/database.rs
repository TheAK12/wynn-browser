// ─────────────────────────────────────────────────────────────────────
// database.rs  –  SQLite connection management for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// Provides a thread-local database connection and schema initialisation.
// The database file lives under `$XDG_DATA_HOME/wynn-browser/wynn.db`
// (typically `~/.local/share/wynn-browser/wynn.db`).
//
// Tables:
//
//   history    – visited URLs with title, visit count, timestamps.
//   bookmarks  – saved URLs with title and optional folder.
//   settings   – key-value store for user preferences.
//   workspaces – named tab groups with colour and ordering.
//   passwords  – saved login credentials (encrypted).
// ─────────────────────────────────────────────────────────────────────

use rusqlite::{Connection, Result as SqlResult};
use std::cell::RefCell;
use std::path::PathBuf;

/// Return the directory where Wynn Browser stores its data.
///
/// Respects `$XDG_DATA_HOME`; falls back to `~/.local/share`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = dirs_fallback();
            home.push(".local/share");
            home
        });
    base.join("wynn-browser")
}

/// Simple home-directory fallback (no extra crate needed).
fn dirs_fallback() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Open (or create) the database and run migrations.
fn open_database() -> SqlResult<Connection> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).expect("Failed to create data directory");

    let db_path = dir.join("wynn.db");
    let conn = Connection::open(db_path)?;

    // Enable WAL mode for better concurrent-read performance.
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;

    // ── Schema migrations ───────────────────────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS history (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            url         TEXT    NOT NULL,
            title       TEXT    NOT NULL DEFAULT '',
            visit_count INTEGER NOT NULL DEFAULT 1,
            last_visit  TEXT    NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_history_url
            ON history(url);
        CREATE INDEX IF NOT EXISTS idx_history_last_visit
            ON history(last_visit DESC);

        CREATE TABLE IF NOT EXISTS bookmarks (
            id      INTEGER PRIMARY KEY AUTOINCREMENT,
            url     TEXT    NOT NULL,
            title   TEXT    NOT NULL DEFAULT '',
            folder  TEXT    NOT NULL DEFAULT '',
            created TEXT    NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_bookmarks_url
            ON bookmarks(url);

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS workspaces (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            name     TEXT    NOT NULL,
            color    TEXT    NOT NULL DEFAULT '#3584e4',
            position INTEGER NOT NULL DEFAULT 0,
            created  TEXT    NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS passwords (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            origin       TEXT    NOT NULL,
            username     TEXT    NOT NULL DEFAULT '',
            password_enc TEXT    NOT NULL,
            created      TEXT    NOT NULL DEFAULT (datetime('now')),
            last_used    TEXT    NOT NULL DEFAULT (datetime('now')),
            use_count    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(origin, username)
        );

        CREATE INDEX IF NOT EXISTS idx_passwords_origin
            ON passwords(origin);
        ",
    )?;

    // Ensure there is always at least one workspace ("Default").
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))?;
    if count == 0 {
        conn.execute(
            "INSERT INTO workspaces (name, color, position) VALUES ('Default', '#3584e4', 0)",
            [],
        )?;
    }

    Ok(conn)
}

// ── Thread-local singleton ──────────────────────────────────────────

thread_local! {
    static DB: RefCell<Connection> = RefCell::new(
        open_database().expect("Failed to open Wynn Browser database")
    );
}

/// Execute a closure with a reference to the database connection.
///
/// All database operations should go through this function so we
/// maintain a single connection per thread (GTK is single-threaded).
pub fn with_db<F, R>(f: F) -> R
where
    F: FnOnce(&Connection) -> R,
{
    DB.with(|cell| f(&cell.borrow()))
}
