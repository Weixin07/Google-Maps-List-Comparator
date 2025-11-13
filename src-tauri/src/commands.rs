use serde::Serialize;

use crate::config::PublicAppConfig;
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
