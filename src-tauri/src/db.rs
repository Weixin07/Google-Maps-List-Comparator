use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlcipher::ffi::ErrorCode;
use rusqlcipher::{Connection, Error as SqlcipherError, OpenFlags};
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
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
    let connection =
        Connection::open_with_flags_and_key(db_path, flags, passphrase.expose_secret())?;

    configure_cipher(&connection)?;
    run_migrations(&connection)?;
    assert_encrypted(db_path)?;

    Ok(DatabaseContext {
        connection,
        path: db_path.to_path_buf(),
    })
}

fn configure_cipher(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cipher_page_size = 4096;
        PRAGMA kdf_iter = 64000;
        PRAGMA cipher_hmac_algorithm = HMAC_SHA512;
        PRAGMA cipher_kdf_algorithm = PBKDF2_HMAC_SHA512;
        PRAGMA cipher_memory_security = ON;
        "#,
    )?;
    Ok(())
}

fn run_migrations(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        r#"
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
        "#,
    )?;
    Ok(())
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

fn should_attempt_recovery(err: &SqlcipherError, db_path: &Path) -> bool {
    if !db_path.exists() {
        return false;
    }

    match err {
        SqlcipherError::SqliteFailure(code, message) => {
            matches!(code.code, ErrorCode::NotADatabase | ErrorCode::IoErr)
                || message
                    .as_deref()
                    .map(|msg| msg.contains("encrypted") || msg.contains("database disk image is malformed"))
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

pub fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
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
