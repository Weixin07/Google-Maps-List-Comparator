use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::comparison::{
    ComparisonPagination, ComparisonSegment, ComparisonSegmentPage, ComparisonSnapshot,
};
use crate::config::PublicAppConfig;
use crate::google::{DeviceFlowState, DriveFileMetadata, GoogleIdentity, LoopbackFlowState};
use crate::ingestion::{ImportSummary, ListSlot};
use crate::places::NormalizationStats;
use crate::projects::ComparisonProjectRecord;
use crate::settings::{RuntimeSettings, UpdateRuntimeSettingsPayload};
use crate::{AppState, ExportSummary, MapStyleDescriptor};

#[derive(Debug, Serialize)]
pub struct FoundationHealth {
    pub db_path: String,
    pub telemetry_buffer_path: String,
    pub telemetry_queue_depth: usize,
    pub has_encryption_key: bool,
    pub config: PublicAppConfig,
    pub db_bootstrap_recovered: bool,
    pub db_key_lifecycle: String,
    pub settings: RuntimeSettings,
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
        settings: RuntimeSettings,
    ) -> Self {
        Self {
            db_path,
            telemetry_buffer_path,
            telemetry_queue_depth,
            has_encryption_key,
            config,
            db_bootstrap_recovered,
            db_key_lifecycle,
            settings,
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
pub async fn update_runtime_settings(
    state: tauri::State<'_, AppState>,
    payload: UpdateRuntimeSettingsPayload,
) -> Result<RuntimeSettings, String> {
    state
        .update_runtime_settings(payload)
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
pub async fn google_start_loopback_flow(
    state: tauri::State<'_, AppState>,
) -> Result<LoopbackFlowState, String> {
    state
        .start_loopback_flow()
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
pub async fn google_complete_loopback_sign_in(
    state: tauri::State<'_, AppState>,
    timeout_secs: Option<u64>,
) -> Result<GoogleIdentity, String> {
    state
        .complete_loopback_sign_in(timeout_secs)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn google_current_identity(
    state: tauri::State<'_, AppState>,
) -> Result<GoogleIdentity, String> {
    state
        .current_identity()
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn google_keepalive(state: tauri::State<'_, AppState>) -> Result<GoogleIdentity, String> {
    state
        .keepalive_google()
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn google_refresh_status(
    state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    Ok(state.refresh_status_google())
}

#[tauri::command]
pub async fn google_sign_out(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.sign_out_google().map_err(|err| err.to_string())
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
    project_id: Option<i64>,
    slot: String,
    file_id: String,
    file_name: String,
    mime_type: Option<String>,
    modified_time: Option<String>,
    size: Option<u64>,
    md5_checksum: Option<String>,
) -> Result<ImportSummary, String> {
    let parsed_slot = ListSlot::parse(&slot).map_err(|err| err.to_string())?;
    state
        .import_drive_file(
            project_id,
            parsed_slot,
            file_id,
            file_name,
            mime_type,
            modified_time,
            size,
            md5_checksum,
        )
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn drive_save_selection(
    state: tauri::State<'_, AppState>,
    project_id: Option<i64>,
    slot: String,
    file: Option<DriveFileMetadata>,
) -> Result<(), String> {
    let parsed_slot = ListSlot::parse(&slot).map_err(|err| err.to_string())?;
    state
        .save_drive_selection(project_id, parsed_slot, file)
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn refresh_place_details(
    state: tauri::State<'_, AppState>,
    project_id: Option<i64>,
    slot: Option<String>,
    request_id: Option<String>,
) -> Result<Vec<NormalizationStats>, String> {
    let parsed = match slot {
        Some(value) => Some(vec![ListSlot::parse(&value).map_err(|err| err.to_string())?]),
        None => None,
    };
    state
        .refresh_place_details(project_id, parsed, request_id)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn cancel_refresh_queue(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.cancel_refresh_queue().map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn compare_lists(
    state: tauri::State<'_, AppState>,
    project_id: Option<i64>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<ComparisonSnapshot, String> {
    state
        .comparison_snapshot(project_id, Some(ComparisonPagination::new(page, page_size)))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn comparison_segment_page(
    state: tauri::State<'_, AppState>,
    project_id: Option<i64>,
    segment: String,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<ComparisonSegmentPage, String> {
    let parsed_segment = ComparisonSegment::parse(&segment)
        .ok_or_else(|| format!("unsupported comparison segment: {segment}"))?;
    state
        .comparison_segment_page(
            project_id,
            parsed_segment,
            ComparisonPagination::new(page, page_size),
        )
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn list_comparison_projects(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ComparisonProjectRecord>, String> {
    state
        .list_comparison_projects()
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn create_comparison_project(
    state: tauri::State<'_, AppState>,
    name: String,
    activate: Option<bool>,
) -> Result<ComparisonProjectRecord, String> {
    state
        .create_comparison_project(name, activate.unwrap_or(true))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn rename_comparison_project(
    state: tauri::State<'_, AppState>,
    project_id: i64,
    name: String,
) -> Result<ComparisonProjectRecord, String> {
    state
        .rename_comparison_project(project_id, name)
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn set_active_comparison_project(
    state: tauri::State<'_, AppState>,
    project_id: i64,
) -> Result<ComparisonProjectRecord, String> {
    state
        .set_active_comparison_project(project_id)
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn map_style_descriptor(
    state: tauri::State<'_, AppState>,
) -> Result<MapStyleDescriptor, String> {
    Ok(state.map_style_descriptor())
}

#[tauri::command]
pub async fn export_comparison_segment(
    state: tauri::State<'_, AppState>,
    project_id: Option<i64>,
    segment: String,
    format: String,
    destination: String,
    place_ids: Option<Vec<String>>,
) -> Result<ExportSummary, String> {
    let parsed_segment = ComparisonSegment::parse(&segment)
        .ok_or_else(|| format!("unsupported comparison segment: {segment}"))?;
    let path = PathBuf::from(destination);
    state
        .export_comparison_segment(project_id, parsed_segment, &format, place_ids, path)
        .map_err(|err| err.to_string())
}
