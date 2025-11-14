import { render, screen } from "@testing-library/react";
import { vi } from "vitest";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((command: string) => {
    switch (command) {
      case "foundation_health":
        return Promise.resolve({
          db_path: "mock.db",
          telemetry_buffer_path: "buffer.jsonl",
          telemetry_queue_depth: 0,
          has_encryption_key: true,
          db_bootstrap_recovered: false,
          db_key_lifecycle: "created",
          config: {
            telemetry_endpoint: null,
            telemetry_enabled_by_default: true,
            telemetry_flush_interval_ms: 5000,
            telemetry_batch_size: 10,
            telemetry_buffer_max_bytes: 1024,
            telemetry_buffer_max_files: 3,
            places_rate_limit_qps: 3,
            database_file_name: "test.db",
            has_google_places_key: false,
            has_maptiler_key: false,
            drive_import_enabled: true,
            drive_picker_page_size: 5,
          },
        });
      case "compare_lists":
        return Promise.resolve({
          project: {
            id: 1,
            name: "Demo project",
          },
          stats: {
            list_a_count: 0,
            list_b_count: 0,
            overlap_count: 0,
            only_a_count: 0,
            only_b_count: 0,
            pending_a: 0,
            pending_b: 0,
          },
          overlap: [],
          only_a: [],
          only_b: [],
        });
      case "refresh_place_details":
        return Promise.resolve([]);
      case "list_comparison_projects":
        return Promise.resolve([
          {
            id: 1,
            name: "Demo project",
            slug: "demo",
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
            is_active: true,
            list_a_imported_at: null,
            list_b_imported_at: null,
          },
        ]);
      case "set_active_comparison_project":
      case "create_comparison_project":
        return Promise.resolve({
          id: 1,
          name: "Demo project",
          slug: "demo",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          is_active: true,
          list_a_imported_at: null,
          list_b_imported_at: null,
        });
      case "map_style_descriptor":
        return Promise.resolve({ style_url: null });
      case "export_comparison_segment":
        return Promise.resolve({
          path: "demo.csv",
          rows: 0,
          selected: 0,
          format: "csv",
          segment: "overlap",
        });
      default:
        return Promise.resolve(null);
    }
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn().mockResolvedValue("C:/tmp/mock.csv"),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  open: vi.fn().mockResolvedValue(undefined),
}));

describe("App shell", () => {
  it("renders the comparison app title", async () => {
    render(<App />);
    expect(
      screen.getByRole("heading", { name: /maps list comparator/i }),
    ).toBeInTheDocument();
    expect(await screen.findByText(/mock\.db/i)).toBeInTheDocument();
  });
});
