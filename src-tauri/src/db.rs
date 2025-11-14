use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::ffi::ErrorCode;
use rusqlite::{Connection, Error as SqliteError, OpenFlags, OptionalExtension};
use secrecy::{ExposeSecret, SecretString};
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};
use crate::secrets::{SecretLifecycle, SecretVault};

pub const DB_KEY_ALIAS: &str = "sqlcipher-db-key";

pub struct DatabaseContext {
    pub connection: Connection,
    pub path: PathBuf,
}

pub struct DatabaseBootstrap {
    pub context: DatabaseContext,
    pub key_lifecycle: SecretLifecycle,
    pub recovered: bool,
}

pub fn bootstrap<P: AsRef<Path>>(
    data_dir: P,
    database_file: &str,
    vault: &SecretVault,
) -> AppResult<DatabaseBootstrap> {
    let data_dir = data_dir.as_ref();
    std::fs::create_dir_all(data_dir)?;
    let db_path = data_dir.join(database_file);
    let mut key_material = vault.ensure(DB_KEY_ALIAS)?;

    match establish_context(&db_path, key_material.secret()) {
        Ok(context) => {
            info!(
                target: "database_bootstrap",
                path = %db_path.display(),
                lifecycle = key_material.lifecycle().as_str(),
                "SQLCipher context established"
            );
            Ok(DatabaseBootstrap {
                context,
                key_lifecycle: key_material.lifecycle(),
                recovered: false,
            })
        }
        Err(AppError::Database(err)) if should_attempt_recovery(&err, &db_path) => {
            warn!(
                target: "database_bootstrap",
                path = %db_path.display(),
                lifecycle = key_material.lifecycle().as_str(),
                error = %err,
                "encrypted database failed to open, attempting recovery"
            );
            recover_encrypted_store(&db_path)?;
            if key_material.lifecycle() == SecretLifecycle::Retrieved {
                key_material = vault.rotate(DB_KEY_ALIAS)?;
            }
            let context = establish_context(&db_path, key_material.secret())?;
            Ok(DatabaseBootstrap {
                context,
                key_lifecycle: key_material.lifecycle(),
                recovered: true,
            })
        }
        Err(err) => Err(err),
    }
}

fn establish_context(db_path: &Path, passphrase: &SecretString) -> AppResult<DatabaseContext> {
    match establish_context_with_mode(db_path, passphrase, true) {
        Ok(context) => Ok(context),
        Err(err) if is_memory_security_error(&err) => {
            warn!(
                target: "database_bootstrap",
                path = %db_path.display(),
                "cipher_memory_security unsupported; continuing without locked pages"
            );
            establish_context_with_mode(db_path, passphrase, false)
        }
        Err(err) => Err(err),
    }
}

fn establish_context_with_mode(
    db_path: &Path,
    passphrase: &SecretString,
    enforce_memory_security: bool,
) -> AppResult<DatabaseContext> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
    let connection = Connection::open_with_flags(db_path, flags)?;
    apply_pragmas(&connection, passphrase)?;
    configure_cipher(&connection, enforce_memory_security)?;
    run_migrations(&connection)?;
    assert_encrypted(db_path)?;

    Ok(DatabaseContext {
        connection,
        path: db_path.to_path_buf(),
    })
}

fn apply_pragmas(connection: &Connection, passphrase: &SecretString) -> AppResult<()> {
    connection
        .pragma_update(None, "cipher_default_page_size", 4096_i64)
        .map_err(AppError::from)?;
    connection
        .pragma_update(None, "cipher_default_kdf_iter", 64000_i64)
        .map_err(AppError::from)?;
    connection
        .pragma_update(None, "cipher_default_hmac_algorithm", "HMAC_SHA512")
        .map_err(AppError::from)?;
    connection
        .pragma_update(None, "cipher_default_kdf_algorithm", "PBKDF2_HMAC_SHA512")
        .map_err(AppError::from)?;
    connection
        .pragma_update(None, "key", passphrase.expose_secret())
        .map_err(AppError::from)
}

fn configure_cipher(connection: &Connection, enforce_memory_security: bool) -> AppResult<()> {
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    if enforce_memory_security {
        enable_cipher_memory_security(connection)?;
    }
    Ok(())
}

fn enable_cipher_memory_security(connection: &Connection) -> AppResult<()> {
    match connection.execute("PRAGMA cipher_memory_security = ON;", []) {
        Ok(_) => Ok(()),
        Err(SqliteError::SqliteFailure(error, _)) if error.code == ErrorCode::OutOfMemory => {
            warn!(
                target: "database_bootstrap",
                error = ?error,
                "cipher_memory_security not enabled; continuing without locked pages support"
            );
            Ok(())
        }
        Err(err) => Err(AppError::from(err)),
    }
}

fn is_memory_security_error(err: &AppError) -> bool {
    match err {
        AppError::Database(SqliteError::SqliteFailure(code, _)) => {
            code.code == ErrorCode::OutOfMemory
        }
        _ => false,
    }
}

fn run_migrations(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS comparison_projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            slug TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL DEFAULT (DATETIME('now')),
            updated_at TEXT NOT NULL DEFAULT (DATETIME('now')),
            is_active INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1))
        );

        CREATE TABLE IF NOT EXISTS lists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'drive_kml',
            drive_file_id TEXT,
            imported_at TEXT NOT NULL DEFAULT (DATETIME('now'))
        );

        CREATE TABLE IF NOT EXISTS places (
            place_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            formatted_address TEXT,
            lat REAL NOT NULL,
            lng REAL NOT NULL,
            types TEXT,
            last_checked_at TEXT
        );

        CREATE TABLE IF NOT EXISTS list_places (
            list_id INTEGER NOT NULL,
            place_id TEXT NOT NULL,
            assigned_at TEXT NOT NULL DEFAULT (DATETIME('now')),
            PRIMARY KEY (list_id, place_id),
            FOREIGN KEY (list_id) REFERENCES lists(id) ON DELETE CASCADE,
            FOREIGN KEY (place_id) REFERENCES places(place_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS raw_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            list_id INTEGER NOT NULL,
            source_row_hash TEXT NOT NULL,
            raw_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (DATETIME('now')),
            FOREIGN KEY (list_id) REFERENCES lists(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS normalization_cache (
            source_row_hash TEXT PRIMARY KEY,
            place_id TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (DATETIME('now'))
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_raw_items_list_hash ON raw_items(list_id, source_row_hash);
        "#,
    )?;

    ensure_column(
        connection,
        "list_places",
        "assigned_at TEXT NOT NULL DEFAULT (DATETIME('now'))",
    )?;
    ensure_column(
        connection,
        "lists",
        "project_id INTEGER REFERENCES comparison_projects(id)",
    )?;
    ensure_column(connection, "lists", "slot TEXT NOT NULL DEFAULT 'A'")?;
    connection.execute(
        "CREATE INDEX IF NOT EXISTS idx_places_lat_lng ON places(lat, lng)",
        [],
    )?;
    connection.execute("DROP INDEX IF EXISTS idx_lists_name", [])?;
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_lists_project_slot ON lists(project_id, slot)",
        [],
    )?;
    seed_default_project(connection)?;
    Ok(())
}

fn ensure_column(connection: &Connection, table: &str, definition: &str) -> AppResult<()> {
    let column_name = definition
        .split_whitespace()
        .next()
        .ok_or_else(|| AppError::Config(format!("invalid column definition: {definition}")))?;
    if column_exists(connection, table, column_name)? {
        return Ok(());
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {definition}");
    connection.execute(&sql, [])?;
    Ok(())
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = connection.prepare(&pragma)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn assert_encrypted(db_path: &Path) -> AppResult<()> {
    if !db_path.exists() {
        return Err(AppError::Path(format!(
            "expected encrypted database at {}",
            db_path.display()
        )));
    }
    let mut file = File::open(db_path)?;
    let mut header = [0_u8; 16];
    let read = file.read(&mut header)?;
    const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
    if read == SQLITE_MAGIC.len() && &header == SQLITE_MAGIC {
        return Err(AppError::Config(
            "database header is plaintext; SQLCipher key not applied".into(),
        ));
    }
    Ok(())
}

fn should_attempt_recovery(err: &SqliteError, db_path: &Path) -> bool {
    if !db_path.exists() {
        return false;
    }

    match err {
        SqliteError::SqliteFailure(code, message) => {
            matches!(
                code.code,
                ErrorCode::NotADatabase | ErrorCode::SystemIoFailure
            ) || message
                .as_deref()
                .map(|msg| {
                    msg.contains("encrypted") || msg.contains("database disk image is malformed")
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

fn recover_encrypted_store(db_path: &Path) -> AppResult<()> {
    remove_if_exists(db_path)?;
    remove_if_exists(&wal_path(db_path))?;
    remove_if_exists(&shm_path(db_path))?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> AppResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(AppError::Io(err)),
    }
}

fn wal_path(db_path: &Path) -> PathBuf {
    let mut buf = db_path.to_path_buf();
    let appended = format!("{}-wal", db_path.file_name().unwrap().to_string_lossy());
    buf.set_file_name(appended);
    buf
}

fn shm_path(db_path: &Path) -> PathBuf {
    let mut buf = db_path.to_path_buf();
    let appended = format!("{}-shm", db_path.file_name().unwrap().to_string_lossy());
    buf.set_file_name(appended);
    buf
}

#[allow(dead_code)]
pub fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn seed_default_project(connection: &Connection) -> AppResult<()> {
    connection.execute(
        "INSERT INTO comparison_projects (name, slug, is_active)
        SELECT 'Default project', 'default-project', 1
        WHERE NOT EXISTS (SELECT 1 FROM comparison_projects)",
        [],
    )?;

    let active_id = match connection
        .query_row(
            "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?
    {
        Some(id) => id,
        None => {
            let fallback: i64 = connection.query_row(
                "SELECT id FROM comparison_projects ORDER BY id ASC LIMIT 1",
                [],
                |row| row.get(0),
            )?;
            connection.execute(
                "UPDATE comparison_projects SET is_active = CASE WHEN id = ?1 THEN 1 ELSE 0 END",
                [fallback],
            )?;
            fallback
        }
    };

    connection.execute(
        "UPDATE lists SET project_id = ?1 WHERE project_id IS NULL",
        [active_id],
    )?;

    connection.execute(
        "UPDATE lists SET slot = 'B'
        WHERE slot = 'A' AND LOWER(name) LIKE '%b%'",
        [],
    )?;

    connection.execute(
        "UPDATE lists SET slot = UPPER(slot) WHERE slot NOT IN ('A','B')",
        [],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn runs_migrations_and_creates_tables() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "test.db", &vault).unwrap();
        let ctx = bootstrap.context;

        let mut stmt = ctx
            .connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('lists','places','list_places','raw_items','normalization_cache')",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .count();
        assert_eq!(rows, 5);
        assert!(ctx.path.ends_with("test.db"));
        assert!(!bootstrap.recovered);
        assert_eq!(bootstrap.key_lifecycle, SecretLifecycle::Created);
    }

    #[test]
    fn ensures_data_file_is_encrypted() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "cipher.db", &vault).unwrap();
        let mut header = [0_u8; 16];
        let mut file = File::open(&bootstrap.context.path).unwrap();
        file.read_exact(&mut header).unwrap();
        assert_ne!(&header, b"SQLite format 3\0");
    }

    #[test]
    fn recovers_when_key_missing() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let initial = bootstrap(dir.path(), "recover.db", &vault).unwrap();
        drop(initial);

        vault.delete(DB_KEY_ALIAS).unwrap();
        let recovered = bootstrap(dir.path(), "recover.db", &vault).unwrap();
        assert!(recovered.recovered);
        assert_eq!(recovered.key_lifecycle, SecretLifecycle::Created);
        assert!(recovered.context.path.exists());
    }

    #[test]
    fn recovers_when_key_corrupted() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let first = bootstrap(dir.path(), "rotate.db", &vault).unwrap();
        drop(first);

        vault.rotate(DB_KEY_ALIAS).unwrap();
        let recovered = bootstrap(dir.path(), "rotate.db", &vault).unwrap();
        assert!(recovered.recovered);
        assert_eq!(recovered.key_lifecycle, SecretLifecycle::Rotated);
    }
}
