import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { TelemetryAdapter } from "./adapter";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockedInvoke = vi.mocked(invoke);
const flushMicrotasks = () => Promise.resolve().then(() => Promise.resolve());

describe("TelemetryAdapter", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("skips tracking when disabled", async () => {
    const adapter = new TelemetryAdapter({
      flushSize: 1,
      flushIntervalMs: 0,
      enabled: false,
    });
    adapter.track("disabled");
    await flushMicrotasks();
    expect(mockedInvoke).not.toHaveBeenCalled();
    adapter.dispose();
  });

  it("flushes automatically when flushSize is reached", async () => {
    mockedInvoke.mockResolvedValue(undefined);
    const adapter = new TelemetryAdapter({
      flushSize: 2,
      flushIntervalMs: 0,
      enabled: true,
    });
    adapter.track("first");
    adapter.track("second");
    await flushMicrotasks();
    expect(mockedInvoke).toHaveBeenCalledTimes(2);
    adapter.dispose();
  });

  it("flushes immediately when the event requests it", async () => {
    mockedInvoke.mockResolvedValue(undefined);
    const adapter = new TelemetryAdapter({
      flushSize: 5,
      flushIntervalMs: 0,
      enabled: true,
    });
    adapter.track("immediate", {}, { flush: true });
    await flushMicrotasks();
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    adapter.dispose();
  });

  it("retries events after a transient failure and warns once", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    mockedInvoke.mockRejectedValueOnce(new Error("boom"));
    mockedInvoke.mockResolvedValue(undefined);

    const adapter = new TelemetryAdapter({
      flushSize: 2,
      flushIntervalMs: 0,
      enabled: true,
    });
    adapter.track("first");
    adapter.track("second");
    await flushMicrotasks();

    expect(warn).toHaveBeenCalledTimes(1);
    expect(mockedInvoke).toHaveBeenCalledTimes(1);

    await adapter.flush();
    expect(mockedInvoke).toHaveBeenCalledTimes(3);
    adapter.dispose();
    warn.mockRestore();
  });

  it("uses the timer to drain the queue when idle", async () => {
    vi.useFakeTimers();
    mockedInvoke.mockResolvedValue(undefined);
    const adapter = new TelemetryAdapter({
      flushSize: 10,
      flushIntervalMs: 1000,
      enabled: true,
    });
    adapter.track("delayed");
    expect(mockedInvoke).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1000);
    await flushMicrotasks();
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    adapter.dispose();
  });

  it("uploads via fetch when an endpoint is configured", async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetchMock);
    const adapter = new TelemetryAdapter({
      flushSize: 1,
      flushIntervalMs: 0,
      enabled: true,
    });
    adapter.configureUpload({
      endpoint: "https://example.test/capture",
      distinctId: "salt",
      source: "test",
    });
    adapter.track("ui_boot");
    await flushMicrotasks();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(mockedInvoke).not.toHaveBeenCalled();
    adapter.dispose();
  });

  it("hashes place identifiers with the install salt", async () => {
    const adapter = new TelemetryAdapter({
      flushSize: 1,
      flushIntervalMs: 0,
      enabled: true,
    });
    adapter.setInstallSalt("demo-salt");
    const hash = await adapter.hashPlaceId("ChIJN1t_tDeuEmsRUsoyG83frY4");
    const second = await adapter.hashPlaceId("ChIJN1t_tDeuEmsRUsoyG83frY4");
    expect(hash).toEqual(second);
    expect(hash).not.toContain("ChIJN1t_tDeuEmsRUsoyG83frY4");
    adapter.dispose();
  });
});
