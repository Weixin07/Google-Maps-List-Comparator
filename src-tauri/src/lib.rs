mod commands;
mod config;
mod db;
mod errors;
mod google;
mod ingestion;
mod secrets;
mod telemetry;

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use rusqlite::Connection as SqlConnection;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};
use tracing::warn;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::commands::FoundationHealth;
use crate::db::{DatabaseBootstrap, DatabaseContext, DB_KEY_ALIAS};
use crate::errors::{AppError, AppResult};
use crate::secrets::SecretLifecycle;

const VAULT_SERVICE_NAME: &str = "GoogleMapsListComparator";

pub use commands::foundation_health;
pub use config::AppConfig;
pub use db::bootstrap;
pub use google::{DeviceFlowState, DriveFileMetadata, GoogleIdentity, GoogleServices};
pub use ingestion::{enqueue_place_hashes, parse_kml, persist_rows, ImportSummary, ListSlot};
pub use secrets::SecretVault;
pub use telemetry::TelemetryClient;

#[derive(Debug, Serialize, Clone)]
pub struct ImportProgressPayload {
    pub slot: String,
    pub stage: String,
    pub message: String,
    pub progress: f32,
    pub file_name: Option<String>,
    pub error: Option<String>,
}

impl ImportProgressPayload {
    fn new(
        slot: ListSlot,
        stage: impl Into<String>,
        message: impl Into<String>,
        progress: f32,
        file_name: Option<String>,
    ) -> Self {
        Self {
            slot: slot.as_tag().to_string(),
            stage: stage.into(),
            message: message.into(),
            progress: progress.clamp(0.0, 1.0),
            file_name,
            error: None,
        }
    }

    fn error(slot: ListSlot, file_name: Option<String>, message: impl Into<String>) -> Self {
        Self {
            slot: slot.as_tag().to_string(),
            stage: "error".into(),
            message: "Import failed".into(),
            progress: 0.0,
            file_name,
            error: Some(message.into()),
        }
    }
}

pub struct AppState {
    handle: tauri::AppHandle,
    db: Arc<Mutex<SqlConnection>>,
    db_path: PathBuf,
    vault: SecretVault,
    config: AppConfig,
    telemetry: TelemetryClient,
    db_bootstrap_recovered: bool,
    db_key_lifecycle: SecretLifecycle,
    google: Option<GoogleServices>,
}

impl AppState {
    fn initialize(app: &tauri::AppHandle) -> AppResult<Self> {
        init_tracing();
        let config = AppConfig::from_env();
        let vault = SecretVault::new(VAULT_SERVICE_NAME);
        let data_dir = app.path().app_data_dir()?;
        let handle = app.clone();

        std::fs::create_dir_all(&data_dir)?;
        let DatabaseBootstrap {
            context: DatabaseContext { connection, path },
            key_lifecycle,
            recovered,
        } = bootstrap(&data_dir, &config.database_file_name, &vault)?;
        let telemetry = TelemetryClient::new(&data_dir, &config)?;
        let google = GoogleServices::maybe_new(&config, &vault)?;

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
            handle,
            db: Arc::new(Mutex::new(connection)),
            db_path: path,
            vault,
            config,
            telemetry,
            db_bootstrap_recovered: recovered,
            db_key_lifecycle: key_lifecycle,
            google,
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

    pub async fn start_device_flow(&self) -> AppResult<DeviceFlowState> {
        self.google()?.start_device_flow().await
    }

    pub async fn complete_device_flow(
        &self,
        device_code: String,
        interval_secs: u64,
    ) -> AppResult<GoogleIdentity> {
        let identity = self
            .google()?
            .complete_device_flow(&device_code, interval_secs)
            .await?;

        if let Err(err) = self.telemetry.record(
            "signin_success",
            json!({
                "email": identity.email,
                "expires_at": identity.expires_at,
            }),
        ) {
            warn!(?err, "failed to record signin_success telemetry");
        }

        Ok(identity)
    }

    pub async fn list_drive_files(
        &self,
        limit: Option<usize>,
    ) -> AppResult<Vec<DriveFileMetadata>> {
        let files = self.google()?.list_kml_files(limit).await?;
        if let Err(err) = self.telemetry.record(
            "drive_picker_loaded",
            json!({
                "result_count": files.len(),
            }),
        ) {
            warn!(?err, "failed to record drive_picker_loaded telemetry");
        }
        Ok(files)
    }

    pub async fn import_drive_file(
        &self,
        slot: ListSlot,
        file_id: String,
        file_name: String,
    ) -> AppResult<ImportSummary> {
        match self
            .import_drive_file_inner(slot, file_id, file_name.clone())
            .await
        {
            Ok(summary) => Ok(summary),
            Err(err) => {
                self.notify_progress(ImportProgressPayload::error(
                    slot,
                    Some(file_name),
                    err.to_string(),
                ));
                Err(err)
            }
        }
    }

    fn google(&self) -> AppResult<&GoogleServices> {
        self.google
            .as_ref()
            .ok_or_else(|| AppError::Config("Google OAuth is not configured".into()))
    }

    #[allow(dead_code)]
    pub fn _connection(&self) -> Arc<Mutex<SqlConnection>> {
        Arc::clone(&self.db)
    }

    async fn import_drive_file_inner(
        &self,
        slot: ListSlot,
        file_id: String,
        file_name: String,
    ) -> AppResult<ImportSummary> {
        let file_hash = fingerprint(&file_id);
        if let Err(err) = self.telemetry.record(
            "drive_file_selected",
            json!({
                "slot": slot.as_tag(),
                "file_hash": file_hash.clone(),
            }),
        ) {
            warn!(?err, "failed to record drive_file_selected telemetry");
        }
        if let Err(err) = self.telemetry.record(
            "import_started",
            json!({
                "slot": slot.as_tag(),
                "file_hash": file_hash.clone(),
                "file_name": file_name.clone(),
            }),
        ) {
            warn!(?err, "failed to record import_started telemetry");
        }

        self.notify_progress(ImportProgressPayload::new(
            slot,
            "download",
            format!("Downloading {}", file_name),
            0.0,
            Some(file_name.clone()),
        ));

        let progress_label = file_name.clone();
        let mut progress_cb = |received: u64, total: Option<u64>| {
            let pct = total
                .and_then(|t| {
                    if t == 0 {
                        None
                    } else {
                        Some(received as f32 / t as f32)
                    }
                })
                .unwrap_or(0.0);
            self.notify_progress(ImportProgressPayload::new(
                slot,
                "download",
                format!("Downloading {}", progress_label),
                (pct * 0.6).clamp(0.0, 0.6),
                Some(progress_label.clone()),
            ));
        };

        let downloader = self.google()?.clone();
        let bytes = downloader.download_file(&file_id, &mut progress_cb).await?;

        self.notify_progress(ImportProgressPayload::new(
            slot,
            "parse",
            "Parsing KML data",
            0.65,
            Some(file_name.clone()),
        ));

        let rows = parse_kml(&bytes)?;
        self.notify_progress(ImportProgressPayload::new(
            slot,
            "persist",
            format!("Persisting {} rows", rows.len()),
            0.85,
            Some(file_name.clone()),
        ));

        let summary = {
            let mut conn = self.db.lock();
            persist_rows(&mut conn, slot, &file_id, &rows)?
        };

        enqueue_place_hashes(&self.telemetry, slot, &rows)?;

        self.notify_progress(ImportProgressPayload::new(
            slot,
            "complete",
            format!("Imported {} rows for {}", rows.len(), slot.display_name()),
            1.0,
            Some(file_name),
        ));

        if let Err(err) = self.telemetry.record(
            "import_completed",
            json!({
                "slot": slot.as_tag(),
                "file_hash": file_hash,
                "rows": rows.len(),
            }),
        ) {
            warn!(?err, "failed to record import_completed telemetry");
        }

        Ok(summary)
    }

    fn notify_progress(&self, payload: ImportProgressPayload) {
        if let Err(err) = self.handle.emit("import://progress", payload) {
            warn!(?err, "failed to emit import progress");
        }
    }
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    STANDARD_NO_PAD.encode(hasher.finalize())
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
            commands::record_telemetry_event,
            commands::google_start_device_flow,
            commands::google_complete_sign_in,
            commands::drive_list_kml_files,
            commands::drive_import_kml
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
