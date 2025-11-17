mod commands;
mod comparison;
mod config;
mod db;
mod errors;
mod google;
mod ingestion;
mod places;
mod projects;
mod secrets;
mod settings;
mod telemetry;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use csv::WriterBuilder;
use reqwest::StatusCode;
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
use crate::comparison::{ComparisonSegment, ComparisonSnapshot, PlaceComparisonRow};
use crate::db::{DatabaseBootstrap, DatabaseContext, DB_KEY_ALIAS};
use crate::errors::{AppError, AppResult};
use crate::places::{NormalizationProgress, NormalizationStats, PlaceNormalizer};
use crate::projects::ComparisonProjectRecord;
use crate::secrets::SecretLifecycle;
use crate::settings::{RuntimeSettings, UpdateRuntimeSettingsPayload, UserSettings};
use secrecy::ExposeSecret;

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
    pub details: Option<Vec<String>>,
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
            details: None,
        }
    }

    fn error(
        slot: ListSlot,
        file_name: Option<String>,
        message: impl Into<String>,
        details: Option<Vec<String>>,
    ) -> Self {
        let summary = message.into();
        Self {
            slot: slot.as_tag().to_string(),
            stage: "error".into(),
            message: summary.clone(),
            progress: 0.0,
            file_name,
            error: Some(summary),
            details,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct RefreshProgressPayload {
    pub slot: String,
    pub request_id: Option<String>,
    pub stage: String,
    pub processed: usize,
    pub total_rows: usize,
    pub resolved: usize,
    pub pending: usize,
    pub rate_limit_qps: u32,
    pub message: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct MapStyleDescriptor {
    pub style_url: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ExportSummary {
    pub path: String,
    pub rows: usize,
    pub selected: usize,
    pub format: String,
    pub segment: String,
}

pub struct AppState {
    handle: tauri::AppHandle,
    db: Arc<Mutex<SqlConnection>>,
    active_project_id: Arc<Mutex<i64>>,
    db_path: PathBuf,
    vault: SecretVault,
    config: AppConfig,
    settings: Arc<Mutex<UserSettings>>,
    settings_path: PathBuf,
    telemetry: TelemetryClient,
    db_bootstrap_recovered: bool,
    db_key_lifecycle: SecretLifecycle,
    google: Option<GoogleServices>,
    places: PlaceNormalizer,
    refresh_cancel_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl AppState {
    fn initialize(app: &tauri::AppHandle) -> AppResult<Self> {
        init_tracing();
        let config = AppConfig::from_env();
        let vault = SecretVault::new(VAULT_SERVICE_NAME);
        let data_dir = app.path().app_data_dir()?;
        let handle = app.clone();

        std::fs::create_dir_all(&data_dir)?;
        let settings_path = settings::settings_path(&data_dir);
        let settings = UserSettings::load(&settings_path, &config)?;
        let DatabaseBootstrap {
            context: DatabaseContext { connection, path },
            key_lifecycle,
            recovered,
        } = bootstrap(&data_dir, &config.database_file_name, &vault)?;
        let telemetry = TelemetryClient::new(&data_dir, &config)?;
        telemetry.set_enabled(settings.telemetry_enabled);
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

        let db = Arc::new(Mutex::new(connection));
        let initial_project_id = {
            let conn = db.lock();
            projects::active_project_id(&conn)?
        };
        let active_project_id = Arc::new(Mutex::new(initial_project_id));
        let places = PlaceNormalizer::new(Arc::clone(&db), &config);
        places.set_rate_limit(settings.places_rate_limit_qps);
        let settings = Arc::new(Mutex::new(settings));

        Ok(Self {
            handle,
            db,
            active_project_id,
            db_path: path,
            vault,
            config,
            settings,
            settings_path,
            telemetry,
            db_bootstrap_recovered: recovered,
            db_key_lifecycle: key_lifecycle,
            google,
            places,
            refresh_cancel_token: Arc::new(Mutex::new(None)),
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
            self.runtime_settings(),
        ))
    }

    fn runtime_settings(&self) -> RuntimeSettings {
        self.settings.lock().runtime_profile()
    }

    pub fn map_style_descriptor(&self) -> MapStyleDescriptor {
        let style_url = self.config.maptiler_key.as_ref().map(|key| {
            format!(
                "https://api.maptiler.com/maps/streets/style.json?key={}",
                key.expose_secret()
            )
        });
        MapStyleDescriptor { style_url }
    }

    pub fn list_comparison_projects(&self) -> AppResult<Vec<ComparisonProjectRecord>> {
        let conn = self.db.lock();
        projects::list_projects(&conn)
    }

    pub fn create_comparison_project(
        &self,
        name: String,
        activate: bool,
    ) -> AppResult<ComparisonProjectRecord> {
        let record = {
            let conn = self.db.lock();
            projects::create_project(&conn, &name, activate)?
        };
        if record.is_active {
            *self.active_project_id.lock() = record.id;
        }
        Ok(record)
    }

    pub fn set_active_comparison_project(
        &self,
        project_id: i64,
    ) -> AppResult<ComparisonProjectRecord> {
        let record = {
            let conn = self.db.lock();
            projects::set_active_project(&conn, project_id)?;
            projects::project_by_id(&conn, project_id)?
        };
        *self.active_project_id.lock() = project_id;
        Ok(record)
    }

    pub fn active_comparison_project(&self) -> AppResult<ComparisonProjectRecord> {
        let project_id = *self.active_project_id.lock();
        let conn = self.db.lock();
        projects::project_by_id(&conn, project_id)
    }

    pub fn record_telemetry_event(
        &self,
        name: String,
        payload: Value,
        flush: bool,
    ) -> AppResult<()> {
        self.telemetry.record_lossy(name, payload);
        if flush {
            self.telemetry.flush_lossy();
        }
        Ok(())
    }

    pub async fn start_device_flow(&self) -> AppResult<DeviceFlowState> {
        self.google()?.start_device_flow().await
    }

    pub fn comparison_snapshot(&self, project_id: Option<i64>) -> AppResult<ComparisonSnapshot> {
        let resolved = self.resolve_project_id(project_id)?;
        let conn = self.db.lock();
        comparison::compute_snapshot(&conn, resolved)
    }

    pub fn export_comparison_segment(
        &self,
        project_id: Option<i64>,
        segment: ComparisonSegment,
        format: &str,
        selection: Option<Vec<String>>,
        destination: PathBuf,
    ) -> AppResult<ExportSummary> {
        let resolved = self.resolve_project_id(project_id)?;
        let snapshot = {
            let conn = self.db.lock();
            comparison::compute_snapshot(&conn, resolved)?
        };
        let target_rows = snapshot.rows_for_segment(segment);
        let selection_set = selection.map(|ids| ids.into_iter().collect::<HashSet<_>>());
        let filtered: Vec<&PlaceComparisonRow> = target_rows
            .iter()
            .filter(|row| {
                selection_set
                    .as_ref()
                    .map_or(true, |set| set.contains(&row.place_id))
            })
            .collect();
        let selected_count = selection_set.as_ref().map_or(0, |set| set.len());

        if let Some(parent) = destination.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let export_format = ExportFormat::parse(format)?;
        match export_format {
            ExportFormat::Csv => export_csv(&destination, &filtered)?,
            ExportFormat::Json => export_json(&destination, &filtered)?,
        }

        if let Err(err) = self.telemetry.record(
            "export_generated",
            json!({
                "project_id": resolved,
                "segment": segment.as_str(),
                "format": export_format.as_str(),
                "rows": filtered.len(),
                "selected": selected_count,
            }),
        ) {
            warn!(?err, "failed to record export_generated telemetry");
        }

        Ok(ExportSummary {
            path: destination.to_string_lossy().to_string(),
            rows: filtered.len(),
            selected: selected_count.min(filtered.len()),
            format: export_format.as_str().to_string(),
            segment: segment.as_str().to_string(),
        })
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
        project_id: Option<i64>,
        slot: ListSlot,
        file_id: String,
        file_name: String,
    ) -> AppResult<ImportSummary> {
        let resolved_project = self.resolve_project_id(project_id)?;
        let file_hash = fingerprint(&file_id);
        match self
            .import_drive_file_inner(
                resolved_project,
                slot,
                file_id,
                file_name.clone(),
                file_hash.clone(),
            )
            .await
        {
            Ok(summary) => Ok(summary),
            Err(err) => {
                let (summary, details) = describe_import_error(&err);
                let detail_payload = if details.is_empty() {
                    None
                } else {
                    Some(details.clone())
                };
                self.notify_progress(ImportProgressPayload::error(
                    slot,
                    Some(file_name),
                    summary.clone(),
                    detail_payload,
                ));
                if let Err(telemetry_err) = self.telemetry.record(
                    "import_failed",
                    json!({
                        "slot": slot.as_tag(),
                        "file_hash": file_hash.clone(),
                        "summary": summary.clone(),
                        "detail_count": details.len(),
                    }),
                ) {
                    warn!(?telemetry_err, "failed to record import_failed telemetry");
                }
                warn!(
                    slot = slot.as_tag(),
                    file_hash,
                    summary = summary.as_str(),
                    detail_count = details.len(),
                    "drive import failed"
                );
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

    pub async fn refresh_place_details(
        &self,
        project_id: Option<i64>,
        slots: Option<Vec<ListSlot>>,
        request_id: Option<String>,
    ) -> AppResult<Vec<NormalizationStats>> {
        let resolved_project = self.resolve_project_id(project_id)?;
        let targets = slots.unwrap_or_else(|| vec![ListSlot::A, ListSlot::B]);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        {
            let mut guard = self.refresh_cancel_token.lock();
            *guard = Some(cancel_flag.clone());
        }
        let rate_limit = self.places.rate_limit_qps();
        let handle = self.handle.clone();
        let request_token = request_id.clone();
        let notifier = Arc::new(move |progress: NormalizationProgress| {
            let payload = RefreshProgressPayload {
                slot: progress.slot.as_tag().to_string(),
                request_id: request_token.clone(),
                stage: "running".into(),
                processed: progress.processed,
                total_rows: progress.total_rows,
                resolved: progress.resolved,
                pending: progress.total_rows.saturating_sub(progress.processed),
                rate_limit_qps: rate_limit,
                message: format!(
                    "Refreshing {} ({}/{})",
                    progress.slot.display_name(),
                    progress.processed,
                    progress.total_rows
                ),
            };
            if let Err(err) = handle.emit("refresh://progress", payload) {
                warn!(?err, "failed to emit refresh progress");
            }
        });
        let result = self
            .places
            .refresh_slots(
                resolved_project,
                &targets,
                Some(notifier),
                Some(cancel_flag.clone()),
            )
            .await;
        {
            let mut guard = self.refresh_cancel_token.lock();
            guard.take();
        }
        match result {
            Ok(stats) => {
                let cancelled = cancel_flag.load(AtomicOrdering::SeqCst);
                for entry in &stats {
                    let stage = if cancelled && entry.unresolved > 0 {
                        "cancelled"
                    } else {
                        "complete"
                    };
                    self.notify_refresh_progress(RefreshProgressPayload {
                        slot: entry.slot.as_tag().to_string(),
                        request_id: request_id.clone(),
                        stage: stage.into(),
                        processed: entry.total_rows.saturating_sub(entry.unresolved),
                        total_rows: entry.total_rows,
                        resolved: entry.resolved,
                        pending: entry.unresolved,
                        rate_limit_qps: rate_limit,
                        message: if stage == "complete" {
                            format!(
                                "Refreshed {} places for {}",
                                entry.resolved,
                                entry.slot.display_name()
                            )
                        } else {
                            format!(
                                "Cancelled refresh with {} places remaining for {}",
                                entry.unresolved,
                                entry.slot.display_name()
                            )
                        },
                    });
                }
                Ok(stats)
            }
            Err(err) => {
                self.notify_refresh_progress(RefreshProgressPayload {
                    slot: targets
                        .first()
                        .copied()
                        .unwrap_or(ListSlot::A)
                        .as_tag()
                        .to_string(),
                    request_id,
                    stage: "error".into(),
                    processed: 0,
                    total_rows: 0,
                    resolved: 0,
                    pending: 0,
                    rate_limit_qps: rate_limit,
                    message: sanitize_error_copy(&err.to_string()),
                });
                Err(err)
            }
        }
    }

    async fn import_drive_file_inner(
        &self,
        project_id: i64,
        slot: ListSlot,
        file_id: String,
        file_name: String,
        file_hash: String,
    ) -> AppResult<ImportSummary> {
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
            persist_rows(&mut conn, project_id, slot, &file_id, &rows)?
        };

        enqueue_place_hashes(&self.telemetry, slot, &rows)?;

        self.notify_progress(ImportProgressPayload::new(
            slot,
            "normalize",
            "Reconciling Places details",
            0.92,
            Some(file_name.clone()),
        ));

        let normalization = self
            .places
            .normalize_slot(project_id, slot, None, None)
            .await?;

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
                "normalized_rows": normalization.resolved,
                "cache_hits": normalization.cache_hits,
                "places_calls": normalization.places_calls,
                "pending": normalization.unresolved,
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

    fn notify_refresh_progress(&self, payload: RefreshProgressPayload) {
        if let Err(err) = self.handle.emit("refresh://progress", payload) {
            warn!(?err, "failed to emit refresh progress");
        }
    }

    fn resolve_project_id(&self, project_id: Option<i64>) -> AppResult<i64> {
        if let Some(candidate) = project_id {
            {
                let conn = self.db.lock();
                projects::project_by_id(&conn, candidate)?;
            }
            Ok(candidate)
        } else {
            Ok(*self.active_project_id.lock())
        }
    }

    pub fn update_runtime_settings(
        &self,
        payload: UpdateRuntimeSettingsPayload,
    ) -> AppResult<RuntimeSettings> {
        let sanitized = payload.sanitized();
        {
            let mut settings = self.settings.lock();
            let previous_enabled = settings.telemetry_enabled;
            let previous_qps = settings.places_rate_limit_qps;
            settings.apply_patch(&sanitized);
            settings.persist(&self.settings_path)?;
            if settings.telemetry_enabled != previous_enabled {
                self.telemetry.set_enabled(settings.telemetry_enabled);
            }
            if settings.places_rate_limit_qps != previous_qps {
                self.places.set_rate_limit(settings.places_rate_limit_qps);
            }
        }
        Ok(self.runtime_settings())
    }

    pub fn cancel_refresh_queue(&self) -> AppResult<()> {
        if let Some(flag) = self.refresh_cancel_token.lock().clone() {
            flag.store(true, AtomicOrdering::SeqCst);
        }
        Ok(())
    }
}

fn export_csv(path: &Path, rows: &[&PlaceComparisonRow]) -> AppResult<()> {
    let mut writer = WriterBuilder::new().from_path(path)?;
    writer.write_record([
        "place_id",
        "name",
        "formatted_address",
        "lat",
        "lng",
        "types",
        "lists",
    ])?;
    for row in rows {
        let lat = row.lat.to_string();
        let lng = row.lng.to_string();
        let types_joined = row.types.join("|");
        let lists_joined = row
            .lists
            .iter()
            .map(|slot| slot.as_tag())
            .collect::<Vec<_>>()
            .join("|");
        writer.write_record([
            row.place_id.as_str(),
            row.name.as_str(),
            row.formatted_address.as_deref().unwrap_or(""),
            lat.as_str(),
            lng.as_str(),
            types_joined.as_str(),
            lists_joined.as_str(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn export_json(path: &Path, rows: &[&PlaceComparisonRow]) -> AppResult<()> {
    let payload: Vec<_> = rows
        .iter()
        .map(|row| {
            json!({
                "place_id": row.place_id,
                "name": row.name,
                "formatted_address": row.formatted_address,
                "lat": row.lat,
                "lng": row.lng,
                "types": row.types,
                "lists": row.lists.iter().map(|slot| slot.as_tag()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let serialized = serde_json::to_vec_pretty(&payload)?;
    fs::write(path, serialized)?;
    Ok(())
}

enum ExportFormat {
    Csv,
    Json,
}

impl ExportFormat {
    fn parse(value: &str) -> AppResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "csv" => Ok(Self::Csv),
            "json" => Ok(Self::Json),
            other => Err(AppError::Config(format!(
                "unsupported export format: {other}"
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
        }
    }
}

fn describe_import_error(err: &AppError) -> (String, Vec<String>) {
    match err {
        AppError::Http(http_err) => {
            let mut details = Vec::new();
            if let Some(status) = http_err.status() {
                details.push(format!("HTTP status: {}", status));
            }
            if http_err.is_timeout() {
                details.push("The request timed out before Drive responded.".into());
                return ("Google Drive request timed out".into(), details);
            }
            if http_err.is_connect() {
                details.push("The client could not reach the Google Drive endpoint.".into());
                return ("Unable to reach Google Drive".into(), details);
            }
            if matches!(http_err.status(), Some(StatusCode::TOO_MANY_REQUESTS)) {
                details.push("Drive returned 429 Too Many Requests.".into());
                return ("Google Drive rate limit was hit".into(), details);
            }
            if let Some(url) = http_err.url() {
                if let Some(host) = url.host_str() {
                    details.push(format!("Endpoint host: {}", host));
                }
            }
            details.push(format!(
                "Transport: {}",
                sanitize_error_copy(&http_err.to_string())
            ));
            ("Google Drive request failed".into(), details)
        }
        AppError::Parse(reason) => (
            "KML parsing failed".into(),
            vec![format!("Parser: {}", sanitize_error_copy(reason))],
        ),
        AppError::Json(reason) => (
            "Unable to process Drive response".into(),
            vec![format!("JSON error: {}", sanitize_error_copy(&reason.to_string()))],
        ),
        AppError::Io(io_err) => (
            "Failed to persist Drive data locally".into(),
            vec![format!("I/O error: {}", sanitize_error_copy(&io_err.to_string()))],
        ),
        AppError::Database(db_err) => (
            "Database write failed during import".into(),
            vec![format!(
                "SQLite error: {}",
                sanitize_error_copy(&db_err.to_string())
            )],
        ),
        AppError::Config(message) => (
            "Import is not configured correctly".into(),
            vec![sanitize_error_copy(message)],
        ),
        AppError::Keychain(err) => (
            "Secure storage was not accessible".into(),
            vec![format!("Keychain: {}", sanitize_error_copy(&err.to_string()))],
        ),
        _ => (
            "Unexpected import failure".into(),
            vec![sanitize_error_copy(&err.to_string())],
        ),
    }
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    STANDARD_NO_PAD.encode(hasher.finalize())
}

fn sanitize_error_copy(raw: &str) -> String {
    let mut sanitized = redact_segment(raw, "/files/", &['/', '?', '&', ' ']);
    sanitized = redact_segment(&sanitized, "fileId=", &['&', ' ']);
    sanitized = redact_segment(&sanitized, "driveId=", &['&', ' ']);
    sanitized = redact_segment(&sanitized, "resourceKey=", &['&', ' ']);
    sanitized
}

fn redact_segment(value: &str, needle: &str, terminators: &[char]) -> String {
    if !value.contains(needle) {
        return value.to_string();
    }
    let mut result = String::with_capacity(value.len());
    let mut start = 0;
    while let Some(relative) = value[start..].find(needle) {
        let idx = start + relative;
        let head_end = idx + needle.len();
        result.push_str(&value[start..head_end]);
        let tail = &value[head_end..];
        let stop = tail
            .find(|c: char| terminators.contains(&c))
            .unwrap_or_else(|| tail.len());
        result.push_str("<redacted>");
        start = head_end + stop;
    }
    result.push_str(&value[start..]);
    result
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
            commands::drive_import_kml,
            commands::refresh_place_details,
            commands::cancel_refresh_queue,
            commands::compare_lists,
            commands::list_comparison_projects,
            commands::create_comparison_project,
            commands::set_active_comparison_project,
            commands::map_style_descriptor,
            commands::export_comparison_segment,
            commands::update_runtime_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
