export type PublicAppConfig = {
  telemetry_endpoint: string | null;
  telemetry_enabled_by_default: boolean;
  telemetry_flush_interval_ms: number;
  telemetry_batch_size: number;
  telemetry_buffer_max_bytes: number;
  telemetry_buffer_max_files: number;
  places_rate_limit_qps: number;
  database_file_name: string;
  has_google_places_key: boolean;
  has_maptiler_key: boolean;
  drive_import_enabled: boolean;
  drive_picker_page_size: number;
};

export type RuntimeSettings = {
  telemetry_enabled: boolean;
  places_rate_limit_qps: number;
  telemetry_salt: string;
};

export type FoundationHealth = {
  db_path: string;
  telemetry_buffer_path: string;
  telemetry_queue_depth: number;
  has_encryption_key: boolean;
  db_bootstrap_recovered: boolean;
  db_key_lifecycle: string;
  config: PublicAppConfig;
  settings: RuntimeSettings;
};
