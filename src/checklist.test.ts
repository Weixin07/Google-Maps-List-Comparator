import { describe, expect, it } from "vitest";
import type { ChecklistItem } from "./checklist";
import { resolveChecklist } from "./checklist";
import type { FoundationHealth } from "./types/foundation";

const template: ChecklistItem[] = [
  { id: "shell", label: "Shell", note: "tauri", status: "done" },
  { id: "sqlcipher", label: "SQLCipher", note: "db", status: "pending" },
  { id: "telemetry", label: "Telemetry", note: "queue", status: "pending" },
  { id: "ci", label: "CI", note: "workflow", status: "pending" },
];

const baseHealth: FoundationHealth = {
  db_path: "/tmp/db",
  telemetry_buffer_path: "/tmp/buffer",
  telemetry_queue_depth: 0,
  has_encryption_key: true,
  db_bootstrap_recovered: false,
  db_key_lifecycle: "created",
  settings: {
    telemetry_enabled: true,
    places_rate_limit_qps: 3,
    telemetry_salt: "salt",
  },
  config: {
    telemetry_endpoint: null,
    telemetry_enabled_by_default: true,
    telemetry_flush_interval_ms: 1000,
    telemetry_batch_size: 5,
    telemetry_buffer_max_bytes: 1000,
    telemetry_buffer_max_files: 2,
    places_rate_limit_qps: 3,
    database_file_name: "test.db",
    has_google_places_key: false,
    has_maptiler_key: false,
    drive_import_enabled: true,
    drive_picker_page_size: 10,
  },
};

describe("resolveChecklist", () => {
  it("keeps defaults when health is unavailable", () => {
    const result = resolveChecklist(template, { foundationHealth: null, isDevMode: true });
    expect(result).toEqual(template);
  });

  it("marks sqlcipher and telemetry complete when health exists", () => {
    const result = resolveChecklist(template, {
      foundationHealth: baseHealth,
      isDevMode: true,
    });
    const sqlcipher = result.find((item) => item.id === "sqlcipher");
    const telemetry = result.find((item) => item.id === "telemetry");
    expect(sqlcipher?.status).toBe("done");
    expect(telemetry?.status).toBe("done");
  });

  it("keeps CI pending outside dev when template says so", () => {
    const result = resolveChecklist(template, {
      foundationHealth: null,
      isDevMode: false,
    });
    const ci = result.find((item) => item.id === "ci");
    expect(ci?.status).toBe("pending");
  });
});
