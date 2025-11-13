mod commands;
mod config;
mod db;
mod errors;
mod secrets;
mod telemetry;

use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use rusqlite::Connection as SqlConnection;
use serde_json::{json, Value};
use tauri::Manager;
use tracing::warn;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::commands::FoundationHealth;
use crate::config::AppConfig;
use crate::db::{bootstrap, DatabaseBootstrap, DatabaseContext, DB_KEY_ALIAS};
use crate::errors::AppResult;
use crate::secrets::{SecretLifecycle, SecretVault};
use crate::telemetry::TelemetryClient;

const VAULT_SERVICE_NAME: &str = "GoogleMapsListComparator";

pub use commands::foundation_health;

pub struct AppState {
    db: Arc<Mutex<SqlConnection>>,
    db_path: PathBuf,
    vault: SecretVault,
    config: AppConfig,
    telemetry: TelemetryClient,
    db_bootstrap_recovered: bool,
    db_key_lifecycle: SecretLifecycle,
}

impl AppState {
    fn initialize(app: &tauri::AppHandle) -> AppResult<Self> {
        init_tracing();
        let config = AppConfig::from_env();
        let vault = SecretVault::new(VAULT_SERVICE_NAME);
        let data_dir = app.path().app_data_dir()?;

        std::fs::create_dir_all(&data_dir)?;
        let DatabaseBootstrap {
            context: DatabaseContext { connection, path },
            key_lifecycle,
            recovered,
        } = bootstrap(&data_dir, &config.database_file_name, &vault)?;
        let telemetry = TelemetryClient::new(&data_dir, &config)?;

        if let Err(err) = telemetry.record(
            "vault_audit",
            json!({
                "account": DB_KEY_ALIAS,
                "lifecycle": key_lifecycle.as_str(),
                "recovered": recovered,
            }),
        ) {
            warn!(?err, "failed to record vault audit event");
        }
        if let Err(err) = telemetry.record(
            "app_start",
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "telemetry_enabled": config.telemetry_enabled_by_default,
            }),
        ) {
            warn!(?err, "failed to queue telemetry bootstrap event");
        }
        if let Err(err) = telemetry.flush() {
            warn!(?err, "failed to flush telemetry queue");
        }

        Ok(Self {
            db: Arc::new(Mutex::new(connection)),
            db_path: path,
            vault,
            config,
            telemetry,
            db_bootstrap_recovered: recovered,
            db_key_lifecycle: key_lifecycle,
        })
    }

    pub fn foundation_health(&self) -> AppResult<FoundationHealth> {
        let has_key = self.vault.has(DB_KEY_ALIAS)?;
        Ok(FoundationHealth::new(
            self.db_path.to_string_lossy().to_string(),
            self.telemetry.buffer_path().to_string_lossy().to_string(),
            self.telemetry.queue_depth(),
            has_key,
            self.config.public_profile(),
            self.db_bootstrap_recovered,
            self.db_key_lifecycle.as_str().to_string(),
        ))
    }

    pub fn record_telemetry_event(
        &self,
        name: String,
        payload: Value,
        flush: bool,
    ) -> AppResult<()> {
        self.telemetry.record(name, payload)?;
        if flush {
            self.telemetry.flush()?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn _connection(&self) -> Arc<Mutex<SqlConnection>> {
        Arc::clone(&self.db)
    }
}

fn init_tracing() {
    static INIT: OnceCell<()> = OnceCell::new();
    let _ = INIT.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,google_maps_list_comparator=debug"));
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle();
            let state = AppState::initialize(&handle)
                .map_err(|err| -> Box<dyn std::error::Error> { Box::new(err) })?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::foundation_health,
            commands::record_telemetry_event
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
