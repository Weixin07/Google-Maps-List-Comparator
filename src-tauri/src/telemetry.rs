use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

#[cfg(test)]
use std::io;

use crate::config::AppConfig;
#[cfg(test)]
use crate::errors::AppError;
use crate::errors::AppResult;

#[derive(Clone)]
pub struct TelemetryClient {
    enabled: Arc<AtomicBool>,
    queue: Arc<Mutex<Vec<TelemetryEvent>>>,
    buffer_path: PathBuf,
    batch_size: usize,
    max_file_bytes: u64,
    max_file_count: usize,
    #[cfg(test)]
    fault_injector: Option<Arc<TestFaultInjector>>,
}

impl TelemetryClient {
    pub fn new<P: AsRef<Path>>(data_dir: P, config: &AppConfig) -> AppResult<Self> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir)?;
        let buffer_path = data_dir.join("telemetry-buffer.jsonl");
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&buffer_path)?;

        let client = Self {
            enabled: Arc::new(AtomicBool::new(config.telemetry_enabled_by_default)),
            queue: Arc::new(Mutex::new(Vec::new())),
            buffer_path,
            batch_size: config.telemetry_batch_size,
            max_file_bytes: config.telemetry_buffer_max_bytes,
            max_file_count: config.telemetry_buffer_max_files,
            #[cfg(test)]
            fault_injector: None,
        };

        Ok(client)
    }

    pub fn record(&self, name: impl Into<String>, payload: serde_json::Value) -> AppResult<()> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut queue = self.queue.lock();
        queue.push(TelemetryEvent::new(name.into(), payload));
        if queue.len() >= self.batch_size {
            self.persist_locked(&mut queue)?;
        }
        Ok(())
    }

    pub fn flush(&self) -> AppResult<()> {
        let mut queue = self.queue.lock();
        self.persist_locked(&mut queue)
    }

    pub fn queue_depth(&self) -> usize {
        self.queue.lock().len()
    }

    pub fn buffer_path(&self) -> &Path {
        &self.buffer_path
    }

    #[allow(dead_code)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    fn persist_locked(&self, queue: &mut Vec<TelemetryEvent>) -> AppResult<()> {
        if queue.is_empty() {
            return Ok(());
        }

        let (encoded, total_bytes) = encode_batch(queue)?;
        self.write_batch(&encoded, total_bytes)?;
        queue.clear();
        Ok(())
    }

    fn write_batch(&self, encoded: &[Vec<u8>], incoming_bytes: u64) -> AppResult<()> {
        self.rotate_if_needed(incoming_bytes)?;
        #[cfg(test)]
        if self
            .fault_injector
            .as_ref()
            .map(|hooks| hooks.take_disk_full())
            .unwrap_or(false)
        {
            return Err(AppError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "simulated disk full",
            )));
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.buffer_path)?;

        for line in encoded {
            file.write_all(line)?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
        Ok(())
    }

    fn rotate_if_needed(&self, incoming_bytes: u64) -> AppResult<()> {
        let current_size = fs::metadata(&self.buffer_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if current_size + incoming_bytes <= self.max_file_bytes {
            return Ok(());
        }

        #[cfg(test)]
        if self
            .fault_injector
            .as_ref()
            .map(|hooks| hooks.take_permission_error())
            .unwrap_or(false)
        {
            return Err(AppError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "simulated permission error",
            )));
        }

        if self.max_file_count <= 1 {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.buffer_path)?;
            return Ok(());
        }

        let rotated_name = format!(
            "{}-{}.jsonl",
            self.buffer_stem(),
            Utc::now().format("%Y%m%d%H%M%S")
        );
        let rotated_path = self
            .buffer_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(rotated_name);

        if self.buffer_path.exists() {
            fs::rename(&self.buffer_path, &rotated_path)?;
        }

        self.prune_rotations()?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.buffer_path)?;
        Ok(())
    }

    fn prune_rotations(&self) -> AppResult<()> {
        let parent = self.buffer_path.parent().unwrap_or_else(|| Path::new("."));
        let prefix = format!("{}-", self.buffer_stem());
        let mut rotations = fs::read_dir(parent)?
            .filter_map(|entry| {
                entry.ok().and_then(|dir_entry| {
                    let name = dir_entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with(&prefix) && name.ends_with(".jsonl") {
                        Some((
                            dir_entry.path(),
                            dir_entry.metadata().ok()?.modified().ok()?,
                        ))
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();

        rotations.sort_by_key(|(_, modified)| *modified);
        let allowed = self.max_file_count.saturating_sub(1);
        if rotations.len() > allowed {
            let excess = rotations.len() - allowed;
            for (path, _) in rotations.into_iter().take(excess) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }

    fn buffer_stem(&self) -> String {
        self.buffer_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "telemetry-buffer".into())
    }
}

#[derive(Debug, Serialize)]
pub struct TelemetryEvent {
    pub name: String,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

impl TelemetryEvent {
    fn new(name: String, payload: serde_json::Value) -> Self {
        Self {
            name,
            timestamp: Utc::now(),
            payload,
        }
    }
}

fn encode_batch(events: &[TelemetryEvent]) -> AppResult<(Vec<Vec<u8>>, u64)> {
    let mut encoded = Vec::with_capacity(events.len());
    let mut bytes = 0_u64;
    for event in events {
        let line = serde_json::to_vec(event)?;
        bytes += (line.len() + 1) as u64;
        encoded.push(line);
    }
    Ok((encoded, bytes))
}

#[cfg(test)]
#[derive(Default)]
pub struct TestFaultInjector {
    disk_full: AtomicBool,
    permission_error: AtomicBool,
}

#[cfg(test)]
impl TestFaultInjector {
    pub fn fail_next_disk_full(&self) {
        self.disk_full.store(true, Ordering::SeqCst);
    }

    pub fn fail_next_permission_error(&self) {
        self.permission_error.store(true, Ordering::SeqCst);
    }

    fn take_disk_full(&self) -> bool {
        self.disk_full.swap(false, Ordering::SeqCst)
    }

    fn take_permission_error(&self) -> bool {
        self.permission_error.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
impl TelemetryClient {
    pub fn enable_test_hooks(&mut self) -> Arc<TestFaultInjector> {
        let hooks = Arc::new(TestFaultInjector::default());
        self.fault_injector = Some(hooks.clone());
        hooks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn writes_events_to_disk() {
        let dir = tempdir().unwrap();
        let config = AppConfig {
            telemetry_endpoint: None,
            telemetry_enabled_by_default: true,
            telemetry_flush_interval_ms: 1000,
            telemetry_batch_size: 1,
            telemetry_buffer_max_bytes: 1024,
            telemetry_buffer_max_files: 3,
            places_rate_limit_qps: 3,
            database_file_name: "test.db".into(),
            google_places_api_key: None,
            maptiler_key: None,
        };

        let client = TelemetryClient::new(dir.path(), &config).unwrap();
        client
            .record("test_event", json!({ "foo": "bar" }))
            .unwrap();
        client.flush().unwrap();

        let buffer = std::fs::read_to_string(client.buffer_path()).unwrap();
        assert!(buffer.contains("test_event"));
    }

    #[test]
    fn keeps_buffer_across_instances() {
        let dir = tempdir().unwrap();
        let config = test_config();
        {
            let client = TelemetryClient::new(dir.path(), &config).unwrap();
            client.record("first", json!({})).unwrap();
            client.flush().unwrap();
        }

        let client = TelemetryClient::new(dir.path(), &config).unwrap();
        client.record("second", json!({})).unwrap();
        client.flush().unwrap();

        let buffer = std::fs::read_to_string(client.buffer_path()).unwrap();
        assert!(buffer.contains("first"));
        assert!(buffer.contains("second"));
    }

    #[test]
    fn rotates_when_exceeding_capacity() {
        let dir = tempdir().unwrap();
        let mut config = test_config();
        config.telemetry_buffer_max_bytes = 64;
        config.telemetry_batch_size = 1;
        let client = TelemetryClient::new(dir.path(), &config).unwrap();
        for i in 0..4 {
            client
                .record(
                    "big",
                    json!({
                        "payload": "0123456789abcdef0123456789abcdef",
                        "idx": i
                    }),
                )
                .unwrap();
            client.flush().unwrap();
        }
        let rotated = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .ok()
                    .and_then(|e| {
                        let name = e.file_name();
                        Some(name.to_string_lossy().contains("telemetry-buffer-"))
                    })
                    .unwrap_or(false)
            })
            .count();
        assert!(rotated >= 1);
    }

    #[test]
    fn recovers_from_disk_full_error() {
        let dir = tempdir().unwrap();
        let mut config = test_config();
        config.telemetry_batch_size = 1;
        let mut client = TelemetryClient::new(dir.path(), &config).unwrap();
        let hooks = client.enable_test_hooks();
        hooks.fail_next_disk_full();

        let result = client.record("oops", json!({}));
        assert!(result.is_err());
        assert_eq!(client.queue_depth(), 1);
    }

    #[test]
    fn recovers_from_permission_error_during_rotation() {
        let dir = tempdir().unwrap();
        let mut config = test_config();
        config.telemetry_buffer_max_bytes = 1;
        config.telemetry_batch_size = 1;
        let mut client = TelemetryClient::new(dir.path(), &config).unwrap();
        let hooks = client.enable_test_hooks();
        hooks.fail_next_permission_error();

        let result = client.record("rotate", json!({}));
        assert!(result.is_err());
        assert_eq!(client.queue_depth(), 1);
    }

    fn test_config() -> AppConfig {
        AppConfig {
            telemetry_endpoint: None,
            telemetry_enabled_by_default: true,
            telemetry_flush_interval_ms: 1000,
            telemetry_batch_size: 2,
            telemetry_buffer_max_bytes: 1024,
            telemetry_buffer_max_files: 3,
            places_rate_limit_qps: 3,
            database_file_name: "test.db".into(),
            google_places_api_key: None,
            maptiler_key: None,
        }
    }
}
