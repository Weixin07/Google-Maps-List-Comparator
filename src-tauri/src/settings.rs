use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::AppConfig;
use crate::errors::{AppError, AppResult};

const DEFAULT_MAX_QPS: u32 = 10;
const SALT_BYTES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    pub telemetry_enabled: bool,
    pub places_rate_limit_qps: u32,
    pub telemetry_salt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSettings {
    pub telemetry_enabled: bool,
    pub places_rate_limit_qps: u32,
    pub telemetry_salt: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRuntimeSettingsPayload {
    pub telemetry_enabled: Option<bool>,
    pub places_rate_limit_qps: Option<u32>,
}

impl UserSettings {
    pub fn load(path: &Path, config: &AppConfig) -> AppResult<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(settings) => Ok(settings),
                Err(err) => {
                    warn!(
                        target: "settings",
                        error = ?err,
                        "failed to parse settings file; regenerating defaults"
                    );
                    let defaults = Self::from_config(config);
                    defaults.persist(path)?;
                    Ok(defaults)
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let defaults = Self::from_config(config);
                defaults.persist(path)?;
                Ok(defaults)
            }
            Err(err) => Err(AppError::Io(err)),
        }
    }

    pub fn persist(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(self)?;
        fs::write(path, serialized)?;
        Ok(())
    }

    pub fn runtime_profile(&self) -> RuntimeSettings {
        RuntimeSettings {
            telemetry_enabled: self.telemetry_enabled,
            places_rate_limit_qps: self.places_rate_limit_qps,
            telemetry_salt: self.telemetry_salt.clone(),
        }
    }

    pub fn apply_patch(&mut self, payload: &UpdateRuntimeSettingsPayload) {
        if let Some(enabled) = payload.telemetry_enabled {
            self.telemetry_enabled = enabled;
        }
        if let Some(qps) = payload.places_rate_limit_qps {
            self.places_rate_limit_qps = clamp_qps(qps);
        }
    }

    fn from_config(config: &AppConfig) -> Self {
        Self {
            telemetry_enabled: config.telemetry_enabled_by_default,
            places_rate_limit_qps: clamp_qps(config.places_rate_limit_qps),
            telemetry_salt: generate_salt(),
        }
    }
}

impl RuntimeSettings {
    pub fn clamp_rate_limit(mut self) -> Self {
        self.places_rate_limit_qps = clamp_qps(self.places_rate_limit_qps);
        self
    }
}

impl UpdateRuntimeSettingsPayload {
    pub fn sanitized(mut self) -> Self {
        if let Some(qps) = self.places_rate_limit_qps {
            self.places_rate_limit_qps = Some(clamp_qps(qps));
        }
        self
    }
}

fn clamp_qps(value: u32) -> u32 {
    value.clamp(1, DEFAULT_MAX_QPS)
}

fn generate_salt() -> String {
    let mut bytes = vec![0_u8; SALT_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_defaults_when_file_missing() {
        let dir = tempdir().unwrap();
        let config = AppConfig::from_env();
        let path = settings_path(dir.path());
        let settings = UserSettings::load(&path, &config).unwrap();
        assert!(!settings.telemetry_salt.is_empty());
        assert_eq!(
            settings.places_rate_limit_qps,
            clamp_qps(config.places_rate_limit_qps)
        );
    }

    #[test]
    fn persists_updates() {
        let dir = tempdir().unwrap();
        let config = AppConfig::from_env();
        let path = settings_path(dir.path());
        let mut settings = UserSettings::load(&path, &config).unwrap();
        settings.telemetry_enabled = !settings.telemetry_enabled;
        settings.persist(&path).unwrap();
        let roundtrip = UserSettings::load(&path, &config).unwrap();
        assert_eq!(settings.telemetry_enabled, roundtrip.telemetry_enabled);
        assert_eq!(settings.telemetry_salt, roundtrip.telemetry_salt);
    }
}
