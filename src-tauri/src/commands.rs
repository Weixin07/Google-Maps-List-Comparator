use serde::Serialize;
use serde_json::Value;

use crate::config::PublicAppConfig;
use crate::google::{DeviceFlowState, DriveFileMetadata, GoogleIdentity};
use crate::ingestion::{ImportSummary, ListSlot};
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct FoundationHealth {
    pub db_path: String,
    pub telemetry_buffer_path: String,
    pub telemetry_queue_depth: usize,
    pub has_encryption_key: bool,
    pub config: PublicAppConfig,
    pub db_bootstrap_recovered: bool,
    pub db_key_lifecycle: String,
}

impl FoundationHealth {
    pub fn new(
        db_path: String,
        telemetry_buffer_path: String,
        telemetry_queue_depth: usize,
        has_encryption_key: bool,
        config: PublicAppConfig,
        db_bootstrap_recovered: bool,
        db_key_lifecycle: String,
    ) -> Self {
        Self {
            db_path,
            telemetry_buffer_path,
            telemetry_queue_depth,
            has_encryption_key,
            config,
            db_bootstrap_recovered,
            db_key_lifecycle,
        }
    }
}

#[tauri::command]
pub async fn foundation_health(
    state: tauri::State<'_, AppState>,
) -> Result<FoundationHealth, String> {
    state.foundation_health().map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn record_telemetry_event(
    state: tauri::State<'_, AppState>,
    name: String,
    payload: Value,
    flush: Option<bool>,
) -> Result<(), String> {
    state
        .record_telemetry_event(name, payload, flush.unwrap_or(false))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn google_start_device_flow(
    state: tauri::State<'_, AppState>,
) -> Result<DeviceFlowState, String> {
    state
        .start_device_flow()
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn google_complete_sign_in(
    state: tauri::State<'_, AppState>,
    device_code: String,
    interval_secs: Option<u64>,
) -> Result<GoogleIdentity, String> {
    state
        .complete_device_flow(device_code, interval_secs.unwrap_or(5))
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn drive_list_kml_files(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<DriveFileMetadata>, String> {
    state
        .list_drive_files(limit)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn drive_import_kml(
    state: tauri::State<'_, AppState>,
    slot: String,
    file_id: String,
    file_name: String,
) -> Result<ImportSummary, String> {
    let parsed_slot = ListSlot::parse(&slot).map_err(|err| err.to_string())?;
    state
        .import_drive_file(parsed_slot, file_id, file_name)
        .await
        .map_err(|err| err.to_string())
}
