use std::{env, io};

use secrecy::SecretString;
use serde::Serialize;
use tracing::debug;

const DEFAULT_TELEMETRY_BUFFER_MAX_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_TELEMETRY_BUFFER_MAX_FILES: usize = 5;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub telemetry_endpoint: Option<String>,
    pub telemetry_enabled_by_default: bool,
    pub telemetry_flush_interval_ms: u64,
    pub telemetry_batch_size: usize,
    pub telemetry_buffer_max_bytes: u64,
    pub telemetry_buffer_max_files: usize,
    pub places_rate_limit_qps: u32,
    pub database_file_name: String,
    pub google_places_api_key: Option<SecretString>,
    pub maptiler_key: Option<SecretString>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicAppConfig {
    pub telemetry_endpoint: Option<String>,
    pub telemetry_enabled_by_default: bool,
    pub telemetry_flush_interval_ms: u64,
    pub telemetry_batch_size: usize,
    pub telemetry_buffer_max_bytes: u64,
    pub telemetry_buffer_max_files: usize,
    pub places_rate_limit_qps: u32,
    pub database_file_name: String,
    pub has_google_places_key: bool,
    pub has_maptiler_key: bool,
}

impl AppConfig {
    pub fn from_env() -> Self {
        load_dotenv_if_applicable();
        Self {
            telemetry_endpoint: env::var("TELEMETRY_ENDPOINT").ok(),
            telemetry_enabled_by_default: parse_bool("TELEMETRY_ENABLED", true),
            telemetry_flush_interval_ms: parse_u64("TELEMETRY_FLUSH_INTERVAL_MS", 5_000),
            telemetry_batch_size: parse_usize("TELEMETRY_BATCH_SIZE", 25),
            telemetry_buffer_max_bytes: parse_u64(
                "TELEMETRY_BUFFER_MAX_BYTES",
                DEFAULT_TELEMETRY_BUFFER_MAX_BYTES,
            ),
            telemetry_buffer_max_files: parse_usize(
                "TELEMETRY_BUFFER_MAX_FILES",
                DEFAULT_TELEMETRY_BUFFER_MAX_FILES,
            )
            .max(1),
            places_rate_limit_qps: parse_u32("PLACES_RATE_LIMIT_QPS", 3),
            database_file_name: env::var("DATABASE_FILE_NAME")
                .unwrap_or_else(|_| "maps-list-comparator.db".to_string()),
            google_places_api_key: env::var("GOOGLE_PLACES_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(SecretString::new),
            maptiler_key: env::var("MAPTILER_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(SecretString::new),
        }
    }

    pub fn public_profile(&self) -> PublicAppConfig {
        PublicAppConfig {
            telemetry_endpoint: self.telemetry_endpoint.clone(),
            telemetry_enabled_by_default: self.telemetry_enabled_by_default,
            telemetry_flush_interval_ms: self.telemetry_flush_interval_ms,
            telemetry_batch_size: self.telemetry_batch_size,
            telemetry_buffer_max_bytes: self.telemetry_buffer_max_bytes,
            telemetry_buffer_max_files: self.telemetry_buffer_max_files,
            places_rate_limit_qps: self.places_rate_limit_qps,
            database_file_name: self.database_file_name.clone(),
            has_google_places_key: self.google_places_api_key.is_some(),
            has_maptiler_key: self.maptiler_key.is_some(),
        }
    }
}

fn load_dotenv_if_applicable() {
    if !should_load_dotenv() {
        debug!("skipping .env load outside dev mode");
        return;
    }

    if let Err(err) = dotenvy::dotenv() {
        match &err {
            dotenvy::Error::Io(io_err) if io_err.kind() == io::ErrorKind::NotFound => {}
            _ => debug!(?err, "unable to load .env file"),
        }
    }
}

fn should_load_dotenv() -> bool {
    cfg!(debug_assertions) || parse_bool("ALLOW_DOTENV", false)
}

fn parse_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

fn parse_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_public_profile_without_secrets() {
        env::set_var("MAPTILER_API_KEY", "secret");
        env::set_var("GOOGLE_PLACES_API_KEY", "secret");
        env::set_var("DATABASE_FILE_NAME", "custom.db");
        env::set_var("TELEMETRY_ENABLED", "false");
        env::set_var("TELEMETRY_BATCH_SIZE", "10");

        let config = AppConfig::from_env();
        let public = config.public_profile();

        assert_eq!(public.database_file_name, "custom.db");
        assert!(!public.telemetry_enabled_by_default);
        assert!(public.has_google_places_key);
        assert!(public.has_maptiler_key);
        assert!(config.google_places_api_key.is_some());
        assert_eq!(
            public.telemetry_buffer_max_bytes,
            DEFAULT_TELEMETRY_BUFFER_MAX_BYTES
        );
        assert_eq!(
            public.telemetry_buffer_max_files,
            DEFAULT_TELEMETRY_BUFFER_MAX_FILES
        );
    }
}
