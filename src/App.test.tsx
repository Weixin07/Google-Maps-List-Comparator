import { render, screen } from "@testing-library/react";
import { vi } from "vitest";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue({
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
    },
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
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
